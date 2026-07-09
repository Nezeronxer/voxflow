//! Глобальный слушатель клавиш: hold-to-talk + двойное-нажатие-защёлка.
//! Windows/Linux используют rdev; macOS использует CGEventTap без перевода
//! keycode → Unicode, потому что HIToolbox требует main-thread queue и падает
//! при таком вызове из event tap callback на новых macOS.
//!
//! Поведение в режиме "hold":
//! - зажал и держишь → запись, пока держишь (отпустил — стоп);
//! - двойной тап → ЗАЩЁЛКА: запись остаётся включённой без удержания;
//! - одиночное нажатие в защёлке → выключить.
//!
//! Режим "toggle": каждое нажатие переключает запись.

use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::AppHandle;

#[cfg(not(target_os = "macos"))]
use rdev::{listen, Event, EventType, Key};

use crate::engine::EngineCmd;
use crate::settings::Settings;

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventType {
    KeyPress(Key),
    KeyRelease(Key),
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    ControlRight,
    ControlLeft,
    Alt,
    AltGr,
    ShiftRight,
    ShiftLeft,
    MetaLeft,
    MetaRight,
    CapsLock,
    Insert,
    ScrollLock,
    Pause,
    PrintScreen,
    NumLock,
    Escape,
    Return,
    Space,
    Tab,
    Backspace,
    Delete,
    Home,
    End,
    PageUp,
    PageDown,
    UpArrow,
    DownArrow,
    LeftArrow,
    RightArrow,
    KeyA,
    KeyB,
    KeyC,
    KeyD,
    KeyE,
    KeyF,
    KeyG,
    KeyH,
    KeyI,
    KeyJ,
    KeyK,
    KeyL,
    KeyM,
    KeyN,
    KeyO,
    KeyP,
    KeyQ,
    KeyR,
    KeyS,
    KeyT,
    KeyU,
    KeyV,
    KeyW,
    KeyX,
    KeyY,
    KeyZ,
    Num0,
    Num1,
    Num2,
    Num3,
    Num4,
    Num5,
    Num6,
    Num7,
    Num8,
    Num9,
    Kp0,
    Kp1,
    Kp2,
    Kp3,
    Kp4,
    Kp5,
    Kp6,
    Kp7,
    Kp8,
    Kp9,
    KpPlus,
    KpMinus,
    KpMultiply,
    KpDivide,
    KpDelete,
    KpReturn,
    Minus,
    Equal,
    LeftBracket,
    RightBracket,
    BackSlash,
    IntlBackslash,
    SemiColon,
    Quote,
    BackQuote,
    Comma,
    Dot,
    Slash,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
}

/// Дольше — это удержание; короче — тап.
const HOLD_MIN: Duration = Duration::from_millis(250);
/// Окно между двумя нажатиями для распознавания двойного. Оно же — задержка
/// ОТЛОЖЕННОГО Stop после короткого тапа (отложенный Stop ждёт ровно это окно).
///
/// Раньше было 450мс → ~450мс тактильного лага на КАЖДОЙ короткой диктовке. Снижено
/// до 300мс: двойной тап человек делает за <300мс, а одиночный короткий тап теперь
/// финализируется на ~150мс раньше. ВАЖНО: окно распознавания двойного (on_press) и
/// задержка отложенного Stop (on_release) — ОДНА И ТА ЖЕ величина, иначе медленный
/// двойной тап мог бы не успеть отменить уже сработавший Stop. Инвариант C2/C4 цел:
/// второй press бампает generation раньше, чем сработает Stop → Stop сам себя отменяет.
const DOUBLE_WINDOW: Duration = Duration::from_millis(300);

struct HotState {
    key_down: bool,
    improve_down: bool,
    /// Клавиша, ФИЗИЧЕСКИ инициировавшая текущий key_down. Release матчится по ней,
    /// а не по target из настроек: если хоткей сменили во время удержания, release
    /// старой клавиши иначе не матчится → key_down навсегда true, запись не
    /// останавливается, а первое нажатие нового хоткея глотается как авто-повтор (P2-6).
    pressed_key: Option<Key>,
    press_at: Option<Instant>,
    last_tap_release: Option<Instant>,
    latched: bool,
    ignore_release: bool,
    /// «Поколение» хоткей-событий. Инкрементируется на каждый press и на каждый
    /// короткий release. Отложенный Stop (запланированный после короткого тапа)
    /// перед отправкой сверяет своё поколение с текущим: если поколение сменилось
    /// (пришёл второй тап двойного нажатия) — Stop сам себя отменяет. Так мы НЕ
    /// рвём запись между тапами защёлки: ни лишнего Stop, ни второго Start —
    /// в движок за весь двойной тап уходит ровно ОДИН Start (C2/C4).
    generation: u64,
}

impl HotState {
    fn new() -> Self {
        HotState {
            key_down: false,
            improve_down: false,
            pressed_key: None,
            press_at: None,
            last_tap_release: None,
            latched: false,
            ignore_release: false,
            generation: 0,
        }
    }
}

pub fn spawn(tx: Sender<EngineCmd>, settings: Arc<Mutex<Settings>>, app: AppHandle) {
    spawn_platform(tx, settings, app);
}

#[cfg(not(target_os = "macos"))]
fn spawn_platform(tx: Sender<EngineCmd>, settings: Arc<Mutex<Settings>>, _app: AppHandle) {
    std::thread::Builder::new()
        .name("voxflow-hotkey".into())
        .spawn(move || {
            let state = Arc::new(Mutex::new(HotState::new()));
            let callback = move |event: Event| {
                let (target, improve, mode) = {
                    let s = settings.lock();
                    (
                        parse_key(&s.hotkey),
                        parse_key(&s.improve_hotkey),
                        s.mode.clone(),
                    )
                };
                let Some(target) = target else {
                    return;
                };
                dispatch(&state, &tx, event.event_type, target, improve, &mode);
            };
            if let Err(err) = listen(callback) {
                log::error!("rdev listen error: {err:?}");
            }
        })
        .expect("spawn hotkey thread");
}

#[cfg(target_os = "macos")]
fn spawn_platform(tx: Sender<EngineCmd>, settings: Arc<Mutex<Settings>>, app: AppHandle) {
    std::thread::Builder::new()
        .name("voxflow-hotkey".into())
        .spawn(move || macos::listen(tx, settings, app))
        .expect("spawn hotkey thread");
}

/// Соотносит событие с хоткеем и вызывает on_press/on_release. Press матчится по
/// текущему target из настроек, а release — по `pressed_key` (если нажатие отслежено):
/// смена хоткея во время удержания не должна терять release старой клавиши (P2-6).
fn dispatch(
    state: &Arc<Mutex<HotState>>,
    tx: &Sender<EngineCmd>,
    event_type: EventType,
    target: Key,
    improve: Option<Key>,
    mode: &str,
) {
    match event_type {
        EventType::KeyPress(Key::Escape) => {
            let _ = tx.send(EngineCmd::Cancel);
        }
        EventType::KeyPress(k) if Some(k) == improve && k != target => {
            let mut st = state.lock();
            if !st.improve_down {
                st.improve_down = true;
                let _ = tx.send(EngineCmd::ImproveSelection);
            }
        }
        EventType::KeyRelease(k) if Some(k) == improve && k != target => {
            state.lock().improve_down = false;
        }
        EventType::KeyPress(k) if k == target => on_press(state, tx, mode, k),
        EventType::KeyRelease(k) => {
            let ours = match state.lock().pressed_key {
                Some(p) => p == k,   // release именно той клавиши, что начала key_down
                None => k == target, // нажатие не отслежено (latch и т.п.) — по target
            };
            if ours {
                on_release(state, tx, mode);
            }
        }
        _ => {}
    }
}

fn on_press(state: &Arc<Mutex<HotState>>, tx: &Sender<EngineCmd>, mode: &str, key: Key) {
    let mut st = state.lock();
    if st.key_down {
        return; // авто-повтор удержания — игнор
    }
    st.key_down = true;
    st.pressed_key = Some(key);
    // Любое нажатие двигает поколение → отменяет ещё не отправленный отложенный Stop.
    st.generation = st.generation.wrapping_add(1);
    let now = Instant::now();

    // Режим toggle: каждое нажатие — переключение.
    if mode == "toggle" {
        let _ = tx.send(EngineCmd::Toggle);
        return;
    }

    // Режим hold + двойное-нажатие-защёлка.
    if st.latched {
        // Уже защёлкнуто — это нажатие выключает запись.
        st.latched = false;
        st.ignore_release = true;
        let _ = tx.send(EngineCmd::Stop);
        return;
    }
    // Двойное нажатие: второе нажатие вскоре после тап-отпускания → ЗАЩЁЛКА ВКЛ.
    if let Some(rel) = st.last_tap_release {
        if now.duration_since(rel) <= DOUBLE_WINDOW {
            // Запись уже идёт от первого тапа, а его отложенный Stop отменён бампом
            // generation выше — поэтому ВТОРОЙ Start НЕ шлём (иначе двойной старт-звук
            // C2 и перезапуск захвата → перекрытие потоков финала C4). Только защёлкиваем.
            st.latched = true;
            st.ignore_release = true;
            st.last_tap_release = None;
            st.press_at = Some(now);
            let _ = tx.send(EngineCmd::HotkeyLatch);
            return;
        }
    }
    // Обычный старт удержания.
    st.press_at = Some(now);
    let _ = tx.send(EngineCmd::Start);
}

fn on_release(state: &Arc<Mutex<HotState>>, tx: &Sender<EngineCmd>, mode: &str) {
    let mut st = state.lock();
    st.key_down = false;
    st.pressed_key = None;
    if mode == "toggle" {
        return;
    }
    if st.ignore_release {
        st.ignore_release = false;
        return;
    }
    if st.latched {
        return; // защёлкнуто — запись продолжается
    }
    let now = Instant::now();
    let held = st
        .press_at
        .map(|p| now.duration_since(p))
        .unwrap_or_default();

    if held < HOLD_MIN {
        // КОРОТКИЙ тап — кандидат на двойное нажатие. НЕ останавливаем запись сразу:
        // если в течение DOUBLE_WINDOW придёт второй тап (защёлка), Stop не нужен.
        // Откладываем Stop на DOUBLE_WINDOW и гейтим его поколением: если за это время
        // придёт любой новый press (бампнет generation), отложенный Stop сам отменится.
        st.last_tap_release = Some(now);
        let my_gen = st.generation;
        let tx2 = tx.clone();
        let state2 = Arc::clone(state);
        std::thread::Builder::new()
            .name("voxflow-tap-stop".into())
            .spawn(move || {
                std::thread::sleep(DOUBLE_WINDOW);
                let s = state2.lock();
                // Отправляем Stop, только если за окно НЕ было нового нажатия
                // (поколение то же), клавиша не зажата снова и мы не защёлкнулись.
                if s.generation == my_gen && !s.key_down && !s.latched {
                    drop(s);
                    let _ = tx2.send(EngineCmd::Stop);
                }
            })
            .ok();
    } else {
        // Долгое удержание (hold-to-talk) — останавливаем сразу, двойного тапа тут нет.
        st.last_tap_release = None;
        let _ = tx.send(EngineCmd::Stop);
    }
}

/// Сопоставление KeyboardEvent.code (из вебвью) → rdev::Key (rdev 0.5.3).
/// ВАЖНО про имена rdev: цифры верхнего ряда = Num0..Num9, нумпад = Kp0..Kp9,
/// Enter = Return, нумпад-Enter = KpReturn, нумпад-точка = KpDelete; F-клавиши
/// ТОЛЬКО F1..F12 (F13+ в rdev 0.5.3 нет). Старые алиасы имён настроек сохранены
/// для обратной совместимости. Набор обязан совпадать с SUPPORTED_HOTKEYS в ui.tsx.
pub fn parse_key(name: &str) -> Option<Key> {
    let k = match name {
        // --- модификаторы (+ старые алиасы настроек) ---
        "ControlRight" | "RightControl" | "RCtrl" => Key::ControlRight,
        "ControlLeft" | "LeftControl" | "LCtrl" => Key::ControlLeft,
        "AltLeft" | "Alt" => Key::Alt,
        "AltRight" | "AltGr" => Key::AltGr,
        "ShiftRight" => Key::ShiftRight,
        "ShiftLeft" => Key::ShiftLeft,
        "MetaLeft" | "WinLeft" | "Super" => Key::MetaLeft,
        "MetaRight" | "WinRight" => Key::MetaRight,
        "CapsLock" => Key::CapsLock,
        // --- спец-клавиши, удобные для hold-to-talk ---
        "Insert" => Key::Insert,
        "ScrollLock" => Key::ScrollLock,
        "Pause" => Key::Pause,
        "PrintScreen" => Key::PrintScreen,
        "NumLock" => Key::NumLock,
        // --- навигация / редактирование ---
        "Escape" => Key::Escape,
        "Enter" => Key::Return,
        "Space" => Key::Space,
        "Tab" => Key::Tab,
        "Backspace" => Key::Backspace,
        "Delete" => Key::Delete,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        // --- буквы ---
        "KeyA" => Key::KeyA,
        "KeyB" => Key::KeyB,
        "KeyC" => Key::KeyC,
        "KeyD" => Key::KeyD,
        "KeyE" => Key::KeyE,
        "KeyF" => Key::KeyF,
        "KeyG" => Key::KeyG,
        "KeyH" => Key::KeyH,
        "KeyI" => Key::KeyI,
        "KeyJ" => Key::KeyJ,
        "KeyK" => Key::KeyK,
        "KeyL" => Key::KeyL,
        "KeyM" => Key::KeyM,
        "KeyN" => Key::KeyN,
        "KeyO" => Key::KeyO,
        "KeyP" => Key::KeyP,
        "KeyQ" => Key::KeyQ,
        "KeyR" => Key::KeyR,
        "KeyS" => Key::KeyS,
        "KeyT" => Key::KeyT,
        "KeyU" => Key::KeyU,
        "KeyV" => Key::KeyV,
        "KeyW" => Key::KeyW,
        "KeyX" => Key::KeyX,
        "KeyY" => Key::KeyY,
        "KeyZ" => Key::KeyZ,
        // --- цифры верхнего ряда (rdev: Num0..Num9) ---
        "Digit0" => Key::Num0,
        "Digit1" => Key::Num1,
        "Digit2" => Key::Num2,
        "Digit3" => Key::Num3,
        "Digit4" => Key::Num4,
        "Digit5" => Key::Num5,
        "Digit6" => Key::Num6,
        "Digit7" => Key::Num7,
        "Digit8" => Key::Num8,
        "Digit9" => Key::Num9,
        // --- нумпад (rdev: Kp0..Kp9 + Kp-операторы, KpReturn) ---
        "Numpad0" => Key::Kp0,
        "Numpad1" => Key::Kp1,
        "Numpad2" => Key::Kp2,
        "Numpad3" => Key::Kp3,
        "Numpad4" => Key::Kp4,
        "Numpad5" => Key::Kp5,
        "Numpad6" => Key::Kp6,
        "Numpad7" => Key::Kp7,
        "Numpad8" => Key::Kp8,
        "Numpad9" => Key::Kp9,
        "NumpadAdd" => Key::KpPlus,
        "NumpadSubtract" => Key::KpMinus,
        "NumpadMultiply" => Key::KpMultiply,
        "NumpadDivide" => Key::KpDivide,
        "NumpadDecimal" => Key::KpDelete, // rdev зовёт нумпад-точку KpDelete
        "NumpadEnter" => Key::KpReturn,
        // --- символьные ---
        "Minus" => Key::Minus,
        "Equal" => Key::Equal,
        "BracketLeft" => Key::LeftBracket,
        "BracketRight" => Key::RightBracket,
        "Backslash" => Key::BackSlash,
        "IntlBackslash" => Key::IntlBackslash,
        "Semicolon" => Key::SemiColon,
        "Quote" => Key::Quote,
        "Backquote" => Key::BackQuote,
        "Comma" => Key::Comma,
        "Period" => Key::Dot,
        "Slash" => Key::Slash,
        // --- F1..F12 (rdev 0.5.3 не имеет F13+) ---
        "F1" => Key::F1,
        "F2" => Key::F2,
        "F3" => Key::F3,
        "F4" => Key::F4,
        "F5" => Key::F5,
        "F6" => Key::F6,
        "F7" => Key::F7,
        "F8" => Key::F8,
        "F9" => Key::F9,
        "F10" => Key::F10,
        "F11" => Key::F11,
        "F12" => Key::F12,
        _ => return None,
    };
    Some(k)
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{dispatch, parse_key, EventType, HotState, Key};
    use crate::engine::EngineCmd;
    use crate::settings::Settings;
    use parking_lot::Mutex;
    use serde_json::json;
    use std::ffi::c_void;
    use std::sync::mpsc::Sender;
    use std::sync::Arc;
    use std::time::Duration;
    use tauri::{AppHandle, Emitter};

    type CGEventRef = *mut c_void;
    type CGEventTapProxy = *mut c_void;
    type CFMachPortRef = *mut c_void;
    type CFRunLoopRef = *mut c_void;
    type CFRunLoopSourceRef = *mut c_void;
    type CFStringRef = *const c_void;

    type CGEventTapCallBack =
        extern "C" fn(CGEventTapProxy, u32, CGEventRef, *mut c_void) -> CGEventRef;

    const K_CG_HID_EVENT_TAP: u32 = 0;
    const K_CG_HEAD_INSERT_EVENT_TAP: u32 = 0;
    const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: u32 = 1;
    const K_CG_EVENT_KEY_DOWN: u32 = 10;
    const K_CG_EVENT_KEY_UP: u32 = 11;
    const K_CG_EVENT_FLAGS_CHANGED: u32 = 12;
    const K_CG_KEYBOARD_EVENT_KEYCODE: u32 = 9;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: u32,
            place: u32,
            options: u32,
            events_of_interest: u64,
            callback: CGEventTapCallBack,
            user_info: *mut c_void,
        ) -> CFMachPortRef;
        fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFRunLoopCommonModes: CFStringRef;
        fn CFRunLoopGetCurrent() -> CFRunLoopRef;
        fn CFMachPortCreateRunLoopSource(
            allocator: *const c_void,
            port: CFMachPortRef,
            order: isize,
        ) -> CFRunLoopSourceRef;
        fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFStringRef);
        fn CFRunLoopRun();
        fn CFRelease(cf: *const c_void);
    }

    struct ListenerCtx {
        state: Arc<Mutex<HotState>>,
        tx: Sender<EngineCmd>,
        settings: Arc<Mutex<Settings>>,
        modifier_down: Mutex<Vec<Key>>,
    }

    pub fn listen(tx: Sender<EngineCmd>, settings: Arc<Mutex<Settings>>, app: AppHandle) {
        let mut requested_permission = false;
        loop {
            if !listen_event_allowed() {
                if crate::macos_permissions::onboarding_active() {
                    crate::engine::dbg_log(
                        "hotkey: waiting while macOS permissions onboarding is active",
                    );
                    std::thread::sleep(Duration::from_secs(2));
                    continue;
                }
                if !requested_permission {
                    crate::engine::dbg_log(
                        "hotkey: Input Monitoring permission missing; requesting kTCCServiceListenEvent",
                    );
                    emit_permission_error(&app);
                    crate::macos_permissions::request_input_monitoring_once();
                    if !crate::macos_permissions::onboarding_active() {
                        open_input_monitoring_settings();
                    }
                    requested_permission = true;
                } else {
                    crate::engine::dbg_log("hotkey: waiting for macOS Input Monitoring permission");
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }

            let ctx = Box::into_raw(Box::new(ListenerCtx {
                state: Arc::new(Mutex::new(HotState::new())),
                tx: tx.clone(),
                settings: settings.clone(),
                modifier_down: Mutex::new(Vec::new()),
            }));

            let mask = (1u64 << K_CG_EVENT_KEY_DOWN)
                | (1u64 << K_CG_EVENT_KEY_UP)
                | (1u64 << K_CG_EVENT_FLAGS_CHANGED);
            let tap = unsafe {
                CGEventTapCreate(
                    K_CG_HID_EVENT_TAP,
                    K_CG_HEAD_INSERT_EVENT_TAP,
                    K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                    mask,
                    callback,
                    ctx.cast(),
                )
            };
            if tap.is_null() {
                crate::engine::dbg_log(
                    "hotkey: CGEventTapCreate failed; grant Input Monitoring permission",
                );
                log::error!(
                    "macOS hotkey listener is unavailable; grant VoxFlow Input Monitoring permission"
                );
                emit_permission_error(&app);
                if !requested_permission && !crate::macos_permissions::onboarding_active() {
                    open_input_monitoring_settings();
                    requested_permission = true;
                }
                unsafe {
                    drop(Box::from_raw(ctx));
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }

            let source = unsafe { CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0) };
            if source.is_null() {
                crate::engine::dbg_log("hotkey: failed to create CGEventTap run loop source");
                log::error!("macOS hotkey listener failed to create run loop source");
                unsafe {
                    CFRelease(tap.cast());
                    drop(Box::from_raw(ctx));
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }

            crate::engine::dbg_log("hotkey: macOS CGEventTap listener ready");

            unsafe {
                CFRunLoopAddSource(CFRunLoopGetCurrent(), source, kCFRunLoopCommonModes);
                CFRelease(source.cast());
                CFRunLoopRun();
            }
            return;
        }
    }

    fn listen_event_allowed() -> bool {
        crate::macos_permissions::input_monitoring_allowed()
    }

    fn emit_permission_error(app: &AppHandle) {
        let _ = app.emit(
            "error",
            json!({
                "message": "Разрешите VoxFlow доступ «Input Monitoring» в macOS Privacy — горячая клавиша подключится автоматически"
            }),
        );
    }

    fn open_input_monitoring_settings() {
        crate::macos_permissions::open_input_monitoring_settings();
    }

    extern "C" fn callback(
        _proxy: CGEventTapProxy,
        event_type: u32,
        event: CGEventRef,
        user_info: *mut c_void,
    ) -> CGEventRef {
        if event.is_null() || user_info.is_null() {
            return event;
        }
        let ctx = unsafe { &*(user_info as *const ListenerCtx) };
        handle_event(ctx, event_type, event);
        event
    }

    fn handle_event(ctx: &ListenerCtx, event_type: u32, event: CGEventRef) {
        let keycode = unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) };
        let Some(key) = key_from_keycode(keycode as u16) else {
            return;
        };
        let event_type = match event_type {
            K_CG_EVENT_KEY_DOWN => EventType::KeyPress(key),
            K_CG_EVENT_KEY_UP => EventType::KeyRelease(key),
            K_CG_EVENT_FLAGS_CHANGED => modifier_event(ctx, key),
            _ => return,
        };
        let (target, improve, mode) = {
            let s = ctx.settings.lock();
            (
                parse_key(&s.hotkey),
                parse_key(&s.improve_hotkey),
                s.mode.clone(),
            )
        };
        let Some(target) = target else {
            return;
        };
        dispatch(&ctx.state, &ctx.tx, event_type, target, improve, &mode);
    }

    fn modifier_event(ctx: &ListenerCtx, key: Key) -> EventType {
        let mut down = ctx.modifier_down.lock();
        if let Some(pos) = down.iter().position(|k| *k == key) {
            down.swap_remove(pos);
            EventType::KeyRelease(key)
        } else {
            down.push(key);
            EventType::KeyPress(key)
        }
    }

    fn key_from_keycode(code: u16) -> Option<Key> {
        let key = match code {
            0 => Key::KeyA,
            1 => Key::KeyS,
            2 => Key::KeyD,
            3 => Key::KeyF,
            4 => Key::KeyH,
            5 => Key::KeyG,
            6 => Key::KeyZ,
            7 => Key::KeyX,
            8 => Key::KeyC,
            9 => Key::KeyV,
            10 => Key::IntlBackslash,
            11 => Key::KeyB,
            12 => Key::KeyQ,
            13 => Key::KeyW,
            14 => Key::KeyE,
            15 => Key::KeyR,
            16 => Key::KeyY,
            17 => Key::KeyT,
            18 => Key::Num1,
            19 => Key::Num2,
            20 => Key::Num3,
            21 => Key::Num4,
            22 => Key::Num6,
            23 => Key::Num5,
            24 => Key::Equal,
            25 => Key::Num9,
            26 => Key::Num7,
            27 => Key::Minus,
            28 => Key::Num8,
            29 => Key::Num0,
            30 => Key::RightBracket,
            31 => Key::KeyO,
            32 => Key::KeyU,
            33 => Key::LeftBracket,
            34 => Key::KeyI,
            35 => Key::KeyP,
            36 => Key::Return,
            37 => Key::KeyL,
            38 => Key::KeyJ,
            39 => Key::Quote,
            40 => Key::KeyK,
            41 => Key::SemiColon,
            42 => Key::BackSlash,
            43 => Key::Comma,
            44 => Key::Slash,
            45 => Key::KeyN,
            46 => Key::KeyM,
            47 => Key::Dot,
            48 => Key::Tab,
            49 => Key::Space,
            50 => Key::BackQuote,
            51 => Key::Backspace,
            53 => Key::Escape,
            54 => Key::MetaRight,
            55 => Key::MetaLeft,
            56 => Key::ShiftLeft,
            57 => Key::CapsLock,
            58 => Key::Alt,
            59 => Key::ControlLeft,
            60 => Key::ShiftRight,
            61 => Key::AltGr,
            62 => Key::ControlRight,
            65 => Key::KpDelete,
            67 => Key::KpMultiply,
            69 => Key::KpPlus,
            71 => Key::NumLock,
            75 => Key::KpDivide,
            76 => Key::KpReturn,
            78 => Key::KpMinus,
            82 => Key::Kp0,
            83 => Key::Kp1,
            84 => Key::Kp2,
            85 => Key::Kp3,
            86 => Key::Kp4,
            87 => Key::Kp5,
            88 => Key::Kp6,
            89 => Key::Kp7,
            91 => Key::Kp8,
            92 => Key::Kp9,
            96 => Key::F5,
            97 => Key::F6,
            98 => Key::F7,
            99 => Key::F3,
            100 => Key::F8,
            101 => Key::F9,
            103 => Key::F11,
            109 => Key::F10,
            111 => Key::F12,
            115 => Key::Home,
            116 => Key::PageUp,
            117 => Key::Delete,
            118 => Key::F4,
            119 => Key::End,
            120 => Key::F2,
            121 => Key::PageDown,
            122 => Key::F1,
            123 => Key::LeftArrow,
            124 => Key::RightArrow,
            125 => Key::DownArrow,
            126 => Key::UpArrow,
            _ => return None,
        };
        Some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{channel, Receiver, TryRecvError};

    fn mk() -> (Arc<Mutex<HotState>>, Sender<EngineCmd>, Receiver<EngineCmd>) {
        let (tx, rx) = channel();
        (Arc::new(Mutex::new(HotState::new())), tx, rx)
    }

    /// Имитирует «держал дольше HOLD_MIN» без реального sleep: откатывает press_at.
    fn backdate_press(state: &Arc<Mutex<HotState>>) {
        state.lock().press_at = Some(Instant::now() - HOLD_MIN);
    }

    fn assert_no_cmd(rx: &Receiver<EngineCmd>) {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    fn dispatch(
        state: &Arc<Mutex<HotState>>,
        tx: &Sender<EngineCmd>,
        event_type: EventType,
        target: Key,
        mode: &str,
    ) {
        super::dispatch(state, tx, event_type, target, None, mode);
    }

    // P2-6: смена хоткея во время удержания. Release СТАРОЙ клавиши обязан штатно
    // завершить запись (матч по pressed_key, не по target), а первое нажатие
    // НОВОГО хоткея — сработать с первого раза (key_down не залип).
    #[test]
    fn target_change_while_held_release_old_key_stops_and_new_key_works() {
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        backdate_press(&state);
        // Настройка сменилась: target теперь B; отпускаем старую клавишу A.
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyB,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        {
            let st = state.lock();
            assert!(!st.key_down);
            assert!(st.pressed_key.is_none());
        }
        // Первое нажатие нового хоткея НЕ проглатывается.
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyB),
            Key::KeyB,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
    }

    // То же для toggle: release старой клавиши после смены target обязан снять
    // key_down, иначе первый press нового хоткея глотается как авто-повтор.
    #[test]
    fn target_change_while_held_toggle_mode() {
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "toggle",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Toggle)));
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyB,
            "toggle",
        );
        assert_no_cmd(&rx); // в toggle release ничего не шлёт
        assert!(!state.lock().key_down);
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyB),
            Key::KeyB,
            "toggle",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Toggle)));
    }

    // Release ЧУЖОЙ клавиши, пока хоткей удерживается, не должен трогать состояние
    // (даже если чужая клавиша совпадает с новым target из настроек).
    #[test]
    fn foreign_release_while_held_is_ignored() {
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        // target уже B, и кто-то отпустил B (обычная печать) — не наш release.
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyB),
            Key::KeyB,
            "hold",
        );
        assert_no_cmd(&rx);
        let st = state.lock();
        assert!(st.key_down);
        assert!(matches!(st.pressed_key, Some(Key::KeyA)));
    }

    // Обычный hold-to-talk без смены настроек: press → Start, release → Stop.
    #[test]
    fn hold_to_talk_basic() {
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        backdate_press(&state);
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        assert!(!state.lock().key_down);
    }

    #[test]
    fn right_option_hotkey_starts_hold_to_talk() {
        assert!(matches!(parse_key("AltRight"), Some(Key::AltGr)));
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::AltGr),
            Key::AltGr,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        backdate_press(&state);
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::AltGr),
            Key::AltGr,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
    }

    #[test]
    fn escape_sends_cancel_without_touching_primary_state() {
        let (state, tx, rx) = mk();
        super::dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::Escape),
            Key::KeyA,
            None,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Cancel)));
        assert!(!state.lock().key_down);
    }

    #[test]
    fn improve_hotkey_sends_once_per_press() {
        let (state, tx, rx) = mk();
        super::dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::F8),
            Key::KeyA,
            Some(Key::F8),
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::ImproveSelection)));
        super::dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::F8),
            Key::KeyA,
            Some(Key::F8),
            "hold",
        );
        assert_no_cmd(&rx);
        super::dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::F8),
            Key::KeyA,
            Some(Key::F8),
            "hold",
        );
        super::dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::F8),
            Key::KeyA,
            Some(Key::F8),
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::ImproveSelection)));
    }

    // Короткий одиночный тап: Stop не мгновенный, а отложенный на DOUBLE_WINDOW.
    #[test]
    fn single_short_tap_sends_deferred_stop() {
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert_no_cmd(&rx); // сразу после тапа Stop ещё не отправлен
        let got = rx.recv_timeout(DOUBLE_WINDOW + Duration::from_millis(300));
        assert!(matches!(got, Ok(EngineCmd::Stop)));
    }

    // Двойной тап → защёлка: один Start на весь цикл, отложенный Stop отменён,
    // release в защёлке игнорируется; следующий press выключает (Stop).
    #[test]
    fn double_tap_latch_then_press_unlatches() {
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        // Второй тап внутри окна — защёлка, ВТОРОГО Start быть не должно.
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(state.lock().latched);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::HotkeyLatch)));
        // Пережидаем окно отложенного Stop первого тапа: он обязан самоотмениться.
        std::thread::sleep(DOUBLE_WINDOW + Duration::from_millis(100));
        assert_no_cmd(&rx);
        // Нажатие в защёлке — выключение.
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert_no_cmd(&rx); // release выключающего нажатия проглочен (ignore_release)
        let st = state.lock();
        assert!(!st.latched && !st.key_down && st.pressed_key.is_none());
    }

    // Защёлка + смена хоткея: release выключающего нажатия старой клавишей всё ещё
    // матчится по pressed_key и корректно гасит ignore_release без лишних команд.
    #[test]
    fn latch_unlatch_press_survives_target_change_before_release() {
        let (state, tx, rx) = mk();
        // Вгоняем в защёлку двойным тапом.
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(state.lock().latched);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::HotkeyLatch)));
        // Выключающее нажатие A; target меняется на B ДО release.
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyB,
            "hold",
        );
        {
            let st = state.lock();
            assert!(!st.key_down && !st.ignore_release && st.pressed_key.is_none());
        }
        // Пережидаем окно: отложенных Stop быть не должно.
        std::thread::sleep(DOUBLE_WINDOW + Duration::from_millis(100));
        assert_no_cmd(&rx);
        // Новый хоткей работает с первого раза.
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyB),
            Key::KeyB,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
    }
}
