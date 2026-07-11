//! Глобальный слушатель клавиш: hold-to-talk + двойное-нажатие-защёлка.
//! Windows/Linux используют rdev; macOS использует CGEventTap без перевода
//! keycode → Unicode, потому что HIToolbox требует main-thread queue и падает
//! при таком вызове из event tap callback на новых macOS.
//!
//! Поведение в режиме "hold":
//! - зажал и держишь → запись, пока держишь (отпустил — стоп);
//! - при включённой опции двойной тап → ЗАЩЁЛКА: запись остаётся
//!   включённой без удержания;
//! - одиночное нажатие в защёлке → выключить.
//!
//! Режим "toggle": каждое нажатие переключает запись.

use std::sync::atomic::AtomicBool;
#[cfg(not(target_os = "macos"))]
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use tauri::AppHandle;
#[cfg(not(target_os = "macos"))]
use tauri::Emitter;

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

/// Только такое очень короткое нажатие считаем тапом-кандидатом.
/// Длительность нужна только для запоминания первого tap; Stop на release
/// всегда уходит сразу.
const QUICK_TAP_MAX: Duration = Duration::from_millis(180);
/// Окно между release первого tap и press второго. Оно не влияет на
/// скорость StopTap: второй press атомарно запускает новую запись и
/// сигнализирует UI о защёлке через StartLatched.
const DOUBLE_WINDOW: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug)]
struct TapCandidate {
    key: Key,
    released_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PressMode {
    Hold,
    Toggle,
}

#[derive(Clone, Copy)]
struct DispatchSnapshot<'a> {
    target: Key,
    improve: Option<Key>,
    mode: &'a str,
    double_tap_latch: bool,
    cancel_active: bool,
}

impl PressMode {
    fn from_setting(mode: &str) -> Self {
        if mode == "toggle" {
            Self::Toggle
        } else {
            Self::Hold
        }
    }
}

struct HotState {
    key_down: bool,
    improve_down: bool,
    /// Physical key that started the active improve action. Capture mode filters
    /// new presses, but must still consume this key's release or future improve
    /// presses would be mistaken for auto-repeat forever.
    improve_pressed_key: Option<Key>,
    /// Клавиша, ФИЗИЧЕСКИ инициировавшая текущий key_down. Release матчится по ней,
    /// а не по target из настроек: если хоткей сменили во время удержания, release
    /// старой клавиши иначе не матчится → key_down навсегда true, запись не
    /// останавливается, а первое нажатие нового хоткея глотается как авто-повтор (P2-6).
    pressed_key: Option<Key>,
    /// Режим фиксируется на press: изменение настройки до физического release
    /// не должно превращать hold в toggle (или наоборот) посреди одного нажатия.
    pressed_mode: Option<PressMode>,
    /// Настройка защёлки тоже фиксируется на press, чтобы release одного
    /// физического нажатия не менял семантику посереди цикла.
    pressed_double_tap_latch: Option<bool>,
    press_at: Option<Instant>,
    /// Кандидат на первый tap привязан к физической клавише. Если настройку
    /// хоткея сменили между tap-ами, новая клавиша не должна подхватывать
    /// старое окно и неожиданно включать latch.
    tap_candidate: Option<TapCandidate>,
    latched: bool,
    ignore_release: bool,
}

impl HotState {
    fn new() -> Self {
        HotState {
            key_down: false,
            improve_down: false,
            improve_pressed_key: None,
            pressed_key: None,
            pressed_mode: None,
            pressed_double_tap_latch: None,
            press_at: None,
            tap_candidate: None,
            latched: false,
            ignore_release: false,
        }
    }
}

pub fn spawn(
    tx: Sender<EngineCmd>,
    settings: Arc<Mutex<Settings>>,
    app: AppHandle,
    cancel_active: Arc<AtomicBool>,
    capture_active: Arc<AtomicBool>,
) {
    spawn_platform(tx, settings, app, cancel_active, capture_active);
}

#[cfg(not(target_os = "macos"))]
fn spawn_platform(
    tx: Sender<EngineCmd>,
    settings: Arc<Mutex<Settings>>,
    app: AppHandle,
    cancel_active: Arc<AtomicBool>,
    capture_active: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("voxflow-hotkey".into())
        .spawn(move || {
            let state = Arc::new(Mutex::new(HotState::new()));
            loop {
                let callback_state = Arc::clone(&state);
                let callback_tx = tx.clone();
                let callback_settings = Arc::clone(&settings);
                let callback_cancel = Arc::clone(&cancel_active);
                let callback_capture = Arc::clone(&capture_active);
                let callback = move |event: Event| {
                    if callback_capture.load(Ordering::SeqCst) {
                        dispatch_release_during_capture(
                            &callback_state,
                            &callback_tx,
                            event.event_type,
                        );
                        return;
                    }
                    let (target, improve, mode, double_tap_latch) = {
                        let s = callback_settings.lock();
                        (
                            parse_key(&s.hotkey),
                            parse_key(&s.improve_hotkey),
                            s.mode.clone(),
                            s.double_tap_latch,
                        )
                    };
                    let Some(target) = target else {
                        return;
                    };
                    let snapshot = DispatchSnapshot {
                        target,
                        improve,
                        mode: &mode,
                        double_tap_latch,
                        cancel_active: callback_cancel.load(Ordering::SeqCst),
                    };
                    dispatch(&callback_state, &callback_tx, event.event_type, snapshot);
                };

                let message = match listen(callback) {
                    Err(err) => {
                        log::error!("rdev listen error: {err:?}; retrying");
                        format!("Глобальная горячая клавиша недоступна ({err:?}). VoxFlow повторит подключение.")
                    }
                    Ok(()) => {
                        log::warn!("rdev listener stopped unexpectedly; retrying");
                        "Глобальная горячая клавиша остановилась. VoxFlow повторит подключение."
                            .to_string()
                    }
                };
                let _ = app.emit("error", serde_json::json!({ "message": message }));
                std::thread::sleep(Duration::from_secs(2));
            }
        })
        .expect("spawn hotkey thread");
}

#[cfg(target_os = "macos")]
fn spawn_platform(
    tx: Sender<EngineCmd>,
    settings: Arc<Mutex<Settings>>,
    app: AppHandle,
    cancel_active: Arc<AtomicBool>,
    capture_active: Arc<AtomicBool>,
) {
    std::thread::Builder::new()
        .name("voxflow-hotkey".into())
        .spawn(move || macos::listen(tx, settings, app, cancel_active, capture_active))
        .expect("spawn hotkey thread");
}

/// Соотносит событие с хоткеем и вызывает on_press/on_release. Press матчится по
/// текущему target из настроек, а release — по `pressed_key` (если нажатие отслежено):
/// смена хоткея во время удержания не должна терять release старой клавиши (P2-6).
fn dispatch(
    state: &Arc<Mutex<HotState>>,
    tx: &Sender<EngineCmd>,
    event_type: EventType,
    snapshot: DispatchSnapshot<'_>,
) {
    let DispatchSnapshot {
        target,
        improve,
        mode,
        double_tap_latch,
        cancel_active,
    } = snapshot;
    match event_type {
        EventType::KeyPress(Key::Escape) => {
            if cancel_active {
                let mut st = state.lock();
                // Сбрасываем защёлку и окно двойного тапа.
                // Если основная клавиша ещё физически удерживается, её release
                // только очистит state и не отправит лишний Stop после Cancel.
                st.tap_candidate = None;
                st.latched = false;
                st.ignore_release = st.key_down;
                st.press_at = None;
                if !st.key_down {
                    st.pressed_key = None;
                    st.pressed_mode = None;
                    st.pressed_double_tap_latch = None;
                }
                drop(st);
                let _ = tx.send(EngineCmd::Cancel);
            }
        }
        EventType::KeyPress(k) if Some(k) == improve && k != target => {
            let mut st = state.lock();
            if !st.improve_down {
                st.improve_down = true;
                st.improve_pressed_key = Some(k);
                let _ = tx.send(EngineCmd::ImproveSelection);
            }
        }
        EventType::KeyRelease(k) => {
            let improve_release = state.lock().improve_pressed_key == Some(k);
            if improve_release {
                let mut st = state.lock();
                st.improve_down = false;
                st.improve_pressed_key = None;
                return;
            }

            // Release обязан закрывать именно отслеженный press. Повторный/
            // orphan key-up не должен слать ещё один Stop и забивать очередь движка.
            let ours = state.lock().pressed_key == Some(k);
            if ours {
                on_release(state, tx, mode);
            }
        }
        EventType::KeyPress(k) if k == target => on_press(state, tx, mode, k, double_tap_latch),
        _ => {}
    }
}

/// Capture mode blocks every fresh global press so assigning a key cannot start
/// an action. Releases belonging to actions that began before capture are the
/// exception: they must finish/clear the tracked action, otherwise `key_down` or
/// `improve_down` remains latched and the newly assigned binding appears broken.
fn dispatch_release_during_capture(
    state: &Arc<Mutex<HotState>>,
    tx: &Sender<EngineCmd>,
    event_type: EventType,
) {
    let EventType::KeyRelease(key) = event_type else {
        return;
    };

    let (primary_release, improve_release) = {
        let st = state.lock();
        (
            st.pressed_key == Some(key),
            st.improve_pressed_key == Some(key),
        )
    };
    if improve_release {
        let mut st = state.lock();
        st.improve_down = false;
        st.improve_pressed_key = None;
    }
    if primary_release {
        // pressed_mode was captured on key-down, so the fallback is unreachable
        // for a valid tracked press and cannot change hold/toggle semantics.
        on_release(state, tx, "hold");
    }
}

fn on_press(
    state: &Arc<Mutex<HotState>>,
    tx: &Sender<EngineCmd>,
    mode: &str,
    key: Key,
    double_tap_latch: bool,
) {
    on_press_at(state, tx, mode, key, double_tap_latch, Instant::now());
}

/// Purely clocked part of key-down handling. Production passes `Instant::now()`,
/// while tests pass an explicit monotonic instant so rapid sequences never need
/// sleeps and cannot become scheduler-dependent.
fn on_press_at(
    state: &Arc<Mutex<HotState>>,
    tx: &Sender<EngineCmd>,
    mode: &str,
    key: Key,
    double_tap_latch: bool,
    now: Instant,
) {
    let mut st = state.lock();
    if st.key_down {
        return; // авто-повтор удержания — игнор
    }
    st.key_down = true;
    st.pressed_key = Some(key);
    let press_mode = PressMode::from_setting(mode);
    st.pressed_mode = Some(press_mode);
    st.pressed_double_tap_latch = Some(double_tap_latch);

    // Режим toggle: каждое нажатие — переключение.
    if press_mode == PressMode::Toggle {
        // Не переносим состояние hold-защёлки через смену режима.
        st.press_at = None;
        st.tap_candidate = None;
        st.latched = false;
        st.ignore_release = false;
        let _ = tx.send(EngineCmd::Toggle);
        return;
    }

    // Режим hold + двойное-нажатие-защёлка.
    if st.latched {
        // Уже защёлкнуто — это нажатие выключает запись.
        st.latched = false;
        st.ignore_release = true;
        st.tap_candidate = None;
        let _ = tx.send(EngineCmd::Stop);
        return;
    }
    if !double_tap_latch {
        // Не оставляем окно от ранее включённой защёлки: после opt-out
        // следующий press всегда обычный hold-start.
        st.tap_candidate = None;
    }
    // Двойное нажатие: второе нажатие вскоре после тап-отпускания → ЗАЩЁЛКА ВКЛ.
    if double_tap_latch {
        if let Some(candidate) = st.tap_candidate {
            let inside_window = now
                .checked_duration_since(candidate.released_at)
                .is_some_and(|elapsed| elapsed <= DOUBLE_WINDOW);
            if candidate.key == key && inside_window {
                // Первый release уже немедленно послал StopTap. Второй press
                // посылает одну команду: движок сначала показывает latch-UI,
                // затем запускает микрофон. Так синхронная часть Start не
                // может отложить анимацию двойного нажатия.
                st.latched = true;
                st.ignore_release = true;
                st.tap_candidate = None;
                st.press_at = Some(now);
                let _ = tx.send(EngineCmd::StartLatched);
                return;
            }
            st.tap_candidate = None;
        }
    }
    // Обычный старт удержания.
    st.press_at = Some(now);
    let _ = tx.send(EngineCmd::Start);
}

fn on_release(state: &Arc<Mutex<HotState>>, tx: &Sender<EngineCmd>, mode: &str) {
    on_release_at(state, tx, mode, Instant::now());
}

fn on_release_at(state: &Arc<Mutex<HotState>>, tx: &Sender<EngineCmd>, mode: &str, now: Instant) {
    let mut st = state.lock();
    // dispatch допускает сюда только release отслеженного press. Защищаемся
    // и здесь, чтобы прямой вызов не мог породить duplicate Stop.
    if !st.key_down || st.pressed_key.is_none() {
        return;
    }
    st.key_down = false;
    let released_key = st.pressed_key.take().expect("tracked key checked above");
    let press_mode = st
        .pressed_mode
        .take()
        .unwrap_or_else(|| PressMode::from_setting(mode));
    let double_tap_latch = st.pressed_double_tap_latch.take().unwrap_or(false);
    if st.ignore_release {
        st.ignore_release = false;
        st.press_at = None;
        return;
    }
    if press_mode == PressMode::Toggle {
        st.press_at = None;
        return;
    }
    if st.latched {
        return; // защёлкнуто — запись продолжается
    }
    let held = st
        .press_at
        .take()
        .map(|p| now.duration_since(p))
        .unwrap_or_default();

    if double_tap_latch && held < QUICK_TAP_MAX {
        // StopTap не откладывает физический release, но не даёт короткому
        // первому tap мигнуть обычной transcribing-анимацией. Кандидат
        // привязан к той же физической клавише.
        st.tap_candidate = Some(TapCandidate {
            key: released_key,
            released_at: now,
        });
        let _ = tx.send(EngineCmd::StopTap);
    } else {
        st.tap_candidate = None;
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

fn parsed_assignable_key(name: &str) -> Option<Key> {
    let key = parse_key(name)?;
    if key == Key::Escape {
        return None;
    }
    #[cfg(target_os = "macos")]
    if matches!(
        key,
        Key::Insert | Key::Pause | Key::PrintScreen | Key::ScrollLock
    ) {
        return None;
    }
    Some(key)
}

/// Проверяет две пользовательские клавиши до записи Settings. `Escape`
/// зарезервирован для Cancel, а сравнение распарсенных Key ловит также старые
/// алиасы вроде ControlRight/RCtrl.
pub fn validate_bindings(settings: &Settings) -> Result<(), String> {
    let primary = parsed_assignable_key(&settings.hotkey).ok_or_else(|| {
        format!(
            "Клавиша диктовки '{}' не поддерживается или зарезервирована",
            settings.hotkey
        )
    })?;
    let improve = parsed_assignable_key(&settings.improve_hotkey).ok_or_else(|| {
        format!(
            "Клавиша улучшения '{}' не поддерживается или зарезервирована",
            settings.improve_hotkey
        )
    })?;
    if primary == improve {
        return Err("Клавиши диктовки и улучшения должны отличаться".into());
    }
    Ok(())
}

/// Детерминированно чинит старые/повреждённые настройки при запуске. Валидные
/// пользовательские значения (включая Right Control на macOS) не меняются.
pub fn repair_bindings(settings: &mut Settings) -> bool {
    if validate_bindings(settings).is_ok() {
        return false;
    }

    let defaults = Settings::default();
    if parsed_assignable_key(&settings.hotkey).is_none() {
        settings.hotkey = defaults.hotkey;
    }
    if parsed_assignable_key(&settings.improve_hotkey).is_none() {
        settings.improve_hotkey = defaults.improve_hotkey;
    }

    let primary = parsed_assignable_key(&settings.hotkey);
    let improve = parsed_assignable_key(&settings.improve_hotkey);
    if primary.is_some() && primary == improve {
        settings.improve_hotkey = if settings.hotkey == "F8" {
            "F7".into()
        } else {
            "F8".into()
        };
    }

    // Defaults and F7/F8 are part of the parser contract on every platform.
    debug_assert!(validate_bindings(settings).is_ok());
    true
}

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        dispatch, dispatch_release_during_capture, parse_key, DispatchSnapshot, EventType,
        HotState, Key,
    };
    use crate::engine::EngineCmd;
    use crate::settings::Settings;
    use parking_lot::Mutex;
    use serde_json::json;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};
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
    const K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE: i32 = 1;

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
        fn CGEventSourceKeyState(state_id: i32, key: u16) -> bool;
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
        cancel_active: Arc<AtomicBool>,
        capture_active: Arc<AtomicBool>,
        modifier_down: Mutex<Vec<Key>>,
    }

    pub fn listen(
        tx: Sender<EngineCmd>,
        settings: Arc<Mutex<Settings>>,
        app: AppHandle,
        cancel_active: Arc<AtomicBool>,
        capture_active: Arc<AtomicBool>,
    ) {
        let mut requested_permission = false;
        let mut reported_tap_failure = false;
        loop {
            // Input Monitoring — единственное разрешение, нужное для event tap.
            // Accessibility может ещё ожидать в onboarding: это не повод
            // оставлять хоткей неактивным. Engine отдельно делает insertion
            // preflight перед открытием микрофона и не даст потерять текст.
            if !listen_event_allowed() {
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
                    log::trace!("hotkey: waiting for macOS Input Monitoring permission");
                }
                // TCC не даёт callback о смене доступа. Короткий polling позволяет
                // первому press сработать максимум через 100 ms после выдачи права,
                // а не через прежние две секунды.
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }

            let ctx = Box::into_raw(Box::new(ListenerCtx {
                state: Arc::new(Mutex::new(HotState::new())),
                tx: tx.clone(),
                settings: settings.clone(),
                cancel_active: cancel_active.clone(),
                capture_active: capture_active.clone(),
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
                if !reported_tap_failure {
                    crate::engine::dbg_log(
                        "hotkey: CGEventTapCreate failed; grant Input Monitoring permission",
                    );
                    log::error!(
                        "macOS hotkey listener is unavailable; grant VoxFlow Input Monitoring permission"
                    );
                    emit_permission_error(&app);
                    reported_tap_failure = true;
                }
                if !requested_permission && !crate::macos_permissions::onboarding_active() {
                    open_input_monitoring_settings();
                    requested_permission = true;
                }
                unsafe {
                    drop(Box::from_raw(ctx));
                }
                // IOHIDCheckAccess can turn positive slightly before event-tap
                // creation starts succeeding. Retry promptly, without flooding
                // the log/error overlay on every attempt.
                std::thread::sleep(Duration::from_millis(100));
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
            K_CG_EVENT_KEY_DOWN => Some(EventType::KeyPress(key)),
            K_CG_EVENT_KEY_UP => Some(EventType::KeyRelease(key)),
            K_CG_EVENT_FLAGS_CHANGED => {
                // flagsChanged сам по себе не говорит down это или up. Простое
                // переключение локального bool ломается на дубле/потерянном callback.
                // Читаем фактическое HID-состояние именно этого keycode, что также сохраняет
                // различие левого/правого Option, Control, Shift и Command.
                let physically_down = unsafe {
                    CGEventSourceKeyState(K_CG_EVENT_SOURCE_STATE_HID_SYSTEM_STATE, keycode as u16)
                };
                modifier_event(ctx, key, physically_down)
            }
            _ => None,
        };
        let Some(event_type) = event_type else {
            return;
        };
        // Modifier state above is still updated while capture is active so a
        // release after reassignment cannot be mistaken for a fresh press.
        if ctx.capture_active.load(Ordering::SeqCst) {
            dispatch_release_during_capture(&ctx.state, &ctx.tx, event_type);
            return;
        }
        let (target, improve, mode, double_tap_latch) = {
            let s = ctx.settings.lock();
            (
                parse_key(&s.hotkey),
                parse_key(&s.improve_hotkey),
                s.mode.clone(),
                s.double_tap_latch,
            )
        };
        let Some(target) = target else {
            return;
        };
        let snapshot = DispatchSnapshot {
            target,
            improve,
            mode: &mode,
            double_tap_latch,
            cancel_active: ctx.cancel_active.load(Ordering::SeqCst),
        };
        dispatch(&ctx.state, &ctx.tx, event_type, snapshot);
    }

    fn modifier_event(ctx: &ListenerCtx, key: Key, physically_down: bool) -> Option<EventType> {
        let mut down = ctx.modifier_down.lock();
        normalize_modifier_transition(&mut down, key, physically_down)
    }

    pub(super) fn normalize_modifier_transition(
        down: &mut Vec<Key>,
        key: Key,
        physically_down: bool,
    ) -> Option<EventType> {
        let position = down.iter().position(|tracked| *tracked == key);
        match (physically_down, position) {
            (true, None) => {
                down.push(key);
                Some(EventType::KeyPress(key))
            }
            (false, Some(pos)) => {
                down.swap_remove(pos);
                Some(EventType::KeyRelease(key))
            }
            // Auto-repeat, duplicate flagsChanged и orphan release не меняют
            // логическое состояние и не порождают команды движку.
            _ => None,
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

    /// Имитирует «держал не меньше QUICK_TAP_MAX» без sleep.
    fn backdate_press(state: &Arc<Mutex<HotState>>) {
        state.lock().press_at = Some(Instant::now() - QUICK_TAP_MAX);
    }

    fn press_at(
        state: &Arc<Mutex<HotState>>,
        tx: &Sender<EngineCmd>,
        key: Key,
        mode: &str,
        double_tap_latch: bool,
        now: Instant,
    ) {
        super::on_press_at(state, tx, mode, key, double_tap_latch, now);
    }

    fn release_at(state: &Arc<Mutex<HotState>>, tx: &Sender<EngineCmd>, mode: &str, now: Instant) {
        super::on_release_at(state, tx, mode, now);
    }

    fn assert_no_cmd(rx: &Receiver<EngineCmd>) {
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn validates_distinct_assignable_bindings() {
        assert!(validate_bindings(&Settings::default()).is_ok());

        let aliases = Settings {
            hotkey: "ControlRight".into(),
            improve_hotkey: "RCtrl".into(),
            ..Settings::default()
        };
        assert!(validate_bindings(&aliases).is_err());
    }

    #[test]
    fn rejects_unknown_escape_and_duplicate_bindings() {
        for (hotkey, improve) in [
            ("MediaPlayPause", "F8"),
            ("Escape", "F8"),
            ("ControlRight", "Escape"),
            ("F8", "F8"),
        ] {
            let settings = Settings {
                hotkey: hotkey.into(),
                improve_hotkey: improve.into(),
                ..Settings::default()
            };
            assert!(
                validate_bindings(&settings).is_err(),
                "{hotkey}/{improve} must be rejected"
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn rejects_keys_missing_from_macos_keycode_map() {
        for hotkey in ["Insert", "Pause", "PrintScreen", "ScrollLock"] {
            let settings = Settings {
                hotkey: hotkey.into(),
                ..Settings::default()
            };
            assert!(validate_bindings(&settings).is_err(), "{hotkey}");
        }
    }

    #[test]
    fn repairs_invalid_or_conflicting_legacy_bindings() {
        let mut invalid = Settings {
            hotkey: "MediaPlayPause".into(),
            improve_hotkey: "Escape".into(),
            ..Settings::default()
        };
        assert!(repair_bindings(&mut invalid));
        assert_eq!(invalid.hotkey, Settings::default().hotkey);
        assert_eq!(invalid.improve_hotkey, "F8");
        assert!(validate_bindings(&invalid).is_ok());

        let mut conflict = Settings {
            hotkey: "F8".into(),
            improve_hotkey: "F8".into(),
            ..Settings::default()
        };
        assert!(repair_bindings(&mut conflict));
        assert_eq!(conflict.hotkey, "F8");
        assert_eq!(conflict.improve_hotkey, "F7");
        assert!(validate_bindings(&conflict).is_ok());
    }

    fn dispatch(
        state: &Arc<Mutex<HotState>>,
        tx: &Sender<EngineCmd>,
        event_type: EventType,
        target: Key,
        mode: &str,
    ) {
        dispatch_with_latch(state, tx, event_type, target, mode, false);
    }

    fn dispatch_with_latch(
        state: &Arc<Mutex<HotState>>,
        tx: &Sender<EngineCmd>,
        event_type: EventType,
        target: Key,
        mode: &str,
        double_tap_latch: bool,
    ) {
        super::dispatch(
            state,
            tx,
            event_type,
            DispatchSnapshot {
                target,
                improve: None,
                mode,
                double_tap_latch,
                cancel_active: false,
            },
        );
    }

    fn dispatch_custom(
        state: &Arc<Mutex<HotState>>,
        tx: &Sender<EngineCmd>,
        event_type: EventType,
        target: Key,
        improve: Option<Key>,
        cancel_active: bool,
    ) {
        super::dispatch(
            state,
            tx,
            event_type,
            DispatchSnapshot {
                target,
                improve,
                mode: "hold",
                double_tap_latch: false,
                cancel_active,
            },
        );
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

    #[test]
    fn capture_filters_new_keys_but_finishes_a_preexisting_hold() {
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

        // Fresh candidate events are inert while the settings UI captures them.
        super::dispatch_release_during_capture(&state, &tx, EventType::KeyPress(Key::KeyB));
        super::dispatch_release_during_capture(&state, &tx, EventType::KeyRelease(Key::KeyB));
        assert_no_cmd(&rx);
        assert!(state.lock().key_down);

        // The release belonging to the action that predated capture still has
        // to finish it; otherwise every future hotkey press is treated as repeat.
        super::dispatch_release_during_capture(&state, &tx, EventType::KeyRelease(Key::KeyA));
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        {
            let st = state.lock();
            assert!(!st.key_down);
            assert!(st.pressed_key.is_none());
            assert!(st.pressed_mode.is_none());
        }

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

    #[test]
    fn hold_press_released_after_switch_to_toggle_still_stops() {
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
            "toggle",
        );

        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        let st = state.lock();
        assert!(!st.key_down);
        assert!(st.pressed_mode.is_none());
    }

    #[test]
    fn toggle_press_released_after_switch_to_hold_does_not_stop() {
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
            Key::KeyA,
            "hold",
        );

        assert_no_cmd(&rx);
        let st = state.lock();
        assert!(!st.key_down);
        assert!(st.pressed_mode.is_none());
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
    fn duplicate_down_and_up_emit_one_hold_cycle() {
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));

        // Auto-repeat / duplicate down while the physical key is held.
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert_no_cmd(&rx);

        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));

        // Duplicate/orphan up after the tracked press is already closed.
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert_no_cmd(&rx);
    }

    #[test]
    fn orphan_release_never_stops_hold_or_toggle() {
        for mode in ["hold", "toggle"] {
            let (state, tx, rx) = mk();
            dispatch(
                &state,
                &tx,
                EventType::KeyRelease(Key::KeyA),
                Key::KeyA,
                mode,
            );
            assert_no_cmd(&rx);
            assert!(!state.lock().key_down);
        }
    }

    #[test]
    fn rapid_hold_cycles_emit_immediate_ordered_pairs_without_late_stop() {
        let (state, tx, rx) = mk();
        for _ in 0..5 {
            dispatch_with_latch(
                &state,
                &tx,
                EventType::KeyPress(Key::KeyA),
                Key::KeyA,
                "hold",
                true,
            );
            assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
            dispatch_with_latch(
                &state,
                &tx,
                EventType::KeyRelease(Key::KeyA),
                Key::KeyA,
                "hold",
                true,
            );
            assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));
            // Отделяем быстрые hold-циклы от жеста double-tap latch.
            state.lock().tap_candidate = None;
        }
        assert_no_cmd(&rx);
    }

    #[test]
    fn toggle_ignores_repeat_and_duplicate_release() {
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
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "toggle",
        );
        assert_no_cmd(&rx);
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "toggle",
        );
        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "toggle",
        );
        assert_no_cmd(&rx);

        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "toggle",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Toggle)));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_modifier_normalization_uses_physical_state_and_deduplicates() {
        let mut down = Vec::new();
        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::AltGr, true),
            Some(EventType::KeyPress(Key::AltGr))
        );
        assert_eq!(down, vec![Key::AltGr]);

        // Duplicate flagsChanged/auto-repeat cannot invert a held modifier to up.
        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::AltGr, true),
            None
        );
        assert_eq!(down, vec![Key::AltGr]);

        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::AltGr, false),
            Some(EventType::KeyRelease(Key::AltGr))
        );
        assert!(down.is_empty());

        // Duplicate/orphan release stays inert instead of becoming a fresh press.
        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::AltGr, false),
            None
        );
        assert!(down.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_left_and_right_modifiers_are_tracked_independently() {
        let mut down = Vec::new();

        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::Alt, true),
            Some(EventType::KeyPress(Key::Alt))
        );
        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::AltGr, true),
            Some(EventType::KeyPress(Key::AltGr))
        );
        assert_eq!(down, vec![Key::Alt, Key::AltGr]);

        // Releasing the left key must not synthesize an up for Right Option,
        // which is the common dictation binding on macOS.
        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::Alt, false),
            Some(EventType::KeyRelease(Key::Alt))
        );
        assert_eq!(down, vec![Key::AltGr]);
        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::AltGr, true),
            None
        );
        assert_eq!(
            super::macos::normalize_modifier_transition(&mut down, Key::AltGr, false),
            Some(EventType::KeyRelease(Key::AltGr))
        );
        assert!(down.is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_rapid_modifier_cycles_preserve_exact_event_order() {
        let mut down = Vec::new();
        let mut normalized = Vec::new();
        for physically_down in [true, true, false, false, true, false] {
            if let Some(event) = super::macos::normalize_modifier_transition(
                &mut down,
                Key::ControlRight,
                physically_down,
            ) {
                normalized.push(event);
            }
        }

        assert_eq!(
            normalized,
            vec![
                EventType::KeyPress(Key::ControlRight),
                EventType::KeyRelease(Key::ControlRight),
                EventType::KeyPress(Key::ControlRight),
                EventType::KeyRelease(Key::ControlRight),
            ]
        );
        assert!(down.is_empty());
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
    fn idle_escape_does_not_send_cancel_or_touch_primary_state() {
        let (state, tx, rx) = mk();
        dispatch_custom(
            &state,
            &tx,
            EventType::KeyPress(Key::Escape),
            Key::KeyA,
            None,
            false,
        );
        assert_no_cmd(&rx);
        assert!(!state.lock().key_down);
    }

    #[test]
    fn active_escape_sends_cancel_and_resets_latch_state() {
        let (state, tx, rx) = mk();
        {
            let mut st = state.lock();
            st.latched = true;
            st.tap_candidate = Some(TapCandidate {
                key: Key::KeyA,
                released_at: Instant::now(),
            });
        }
        dispatch_custom(
            &state,
            &tx,
            EventType::KeyPress(Key::Escape),
            Key::KeyA,
            None,
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Cancel)));
        let st = state.lock();
        assert!(!st.key_down);
        assert!(!st.latched);
        assert!(st.tap_candidate.is_none());
    }

    #[test]
    fn active_escape_while_held_swallows_later_release() {
        let (state, tx, rx) = mk();
        dispatch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));

        dispatch_custom(
            &state,
            &tx,
            EventType::KeyPress(Key::Escape),
            Key::KeyA,
            None,
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Cancel)));

        dispatch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
        );
        assert_no_cmd(&rx);
        let st = state.lock();
        assert!(!st.key_down);
        assert!(!st.ignore_release);
        assert!(st.pressed_mode.is_none());
    }

    #[test]
    fn improve_hotkey_sends_once_per_press() {
        let (state, tx, rx) = mk();
        dispatch_custom(
            &state,
            &tx,
            EventType::KeyPress(Key::F8),
            Key::KeyA,
            Some(Key::F8),
            false,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::ImproveSelection)));
        dispatch_custom(
            &state,
            &tx,
            EventType::KeyPress(Key::F8),
            Key::KeyA,
            Some(Key::F8),
            false,
        );
        assert_no_cmd(&rx);
        dispatch_custom(
            &state,
            &tx,
            EventType::KeyRelease(Key::F8),
            Key::KeyA,
            Some(Key::F8),
            false,
        );
        dispatch_custom(
            &state,
            &tx,
            EventType::KeyPress(Key::F8),
            Key::KeyA,
            Some(Key::F8),
            false,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::ImproveSelection)));
    }

    // Явно выключенная защёлка: даже короткий release сразу финализируется.
    #[test]
    fn explicitly_disabled_latch_stops_short_tap_immediately() {
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
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
    }

    #[test]
    fn latch_setting_is_captured_on_press_and_opt_out_clears_stale_candidate() {
        let t0 = Instant::now();

        // Enabled when the physical press began: its quick release remains a
        // tap candidate even if UI settings are changed before key-up.
        let (state, tx, rx) = mk();
        press_at(&state, &tx, Key::KeyA, "hold", true, t0);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        release_at(&state, &tx, "hold", t0 + Duration::from_millis(20));
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));

        // The next press observes latch=false, clears the old window, and is a
        // normal hold start even though it is physically inside DOUBLE_WINDOW.
        press_at(
            &state,
            &tx,
            Key::KeyA,
            "hold",
            false,
            t0 + Duration::from_millis(40),
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        assert!(state.lock().tap_candidate.is_none());
        assert_no_cmd(&rx);

        // Disabled when another physical press began: enabling the setting
        // before release cannot retroactively convert it to StopTap.
        let (state, tx, rx) = mk();
        press_at(&state, &tx, Key::KeyA, "hold", false, t0);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        release_at(&state, &tx, "hold", t0 + Duration::from_millis(20));
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        assert!(state.lock().tap_candidate.is_none());
        assert_no_cmd(&rx);
    }

    // Даже короткий tap с включённой защёлкой не задерживает release.
    #[test]
    fn enabled_latch_stops_single_quick_tap_immediately() {
        let (state, tx, rx) = mk();
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));
        assert_no_cmd(&rx);
        assert!(state.lock().tap_candidate.is_some());
    }

    // Защёлка включена по умолчанию, но обычная hold-to-talk диктовка не
    // должна попадать в 300-мс окно или ждать фоновый Stop.
    #[test]
    fn enabled_latch_stops_normal_hold_immediately() {
        let (state, tx, rx) = mk();
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        backdate_press(&state);
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        assert_no_cmd(&rx);
    }

    #[test]
    fn tap_and_double_window_boundaries_are_deterministic() {
        let (state, tx, rx) = mk();
        let t0 = Instant::now();
        let first_release = t0 + QUICK_TAP_MAX - Duration::from_nanos(1);

        press_at(&state, &tx, Key::KeyA, "hold", true, t0);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        release_at(&state, &tx, "hold", first_release);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));

        // Inclusive outer boundary: exactly DOUBLE_WINDOW is still a double tap.
        press_at(
            &state,
            &tx,
            Key::KeyA,
            "hold",
            true,
            first_release + DOUBLE_WINDOW,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StartLatched)));
        assert_no_cmd(&rx);
    }

    #[test]
    fn quick_tap_threshold_is_exclusive_and_long_press_never_latches() {
        let (state, tx, rx) = mk();
        let t0 = Instant::now();

        press_at(&state, &tx, Key::KeyA, "hold", true, t0);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        release_at(&state, &tx, "hold", t0 + QUICK_TAP_MAX);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        assert!(state.lock().tap_candidate.is_none());

        press_at(
            &state,
            &tx,
            Key::KeyA,
            "hold",
            true,
            t0 + QUICK_TAP_MAX + Duration::from_nanos(1),
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        assert_no_cmd(&rx);
    }

    #[test]
    fn expired_tap_candidate_starts_plain_hold_without_late_commands() {
        let (state, tx, rx) = mk();
        let t0 = Instant::now();
        let first_release = t0 + Duration::from_millis(20);

        press_at(&state, &tx, Key::KeyA, "hold", true, t0);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        release_at(&state, &tx, "hold", first_release);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));

        press_at(
            &state,
            &tx,
            Key::KeyA,
            "hold",
            true,
            first_release + DOUBLE_WINDOW + Duration::from_nanos(1),
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        assert!(state.lock().tap_candidate.is_none());
        assert_no_cmd(&rx);
    }

    #[test]
    fn tap_candidate_never_crosses_to_a_new_physical_hotkey() {
        let (state, tx, rx) = mk();
        let t0 = Instant::now();
        let first_release = t0 + Duration::from_millis(20);

        press_at(&state, &tx, Key::KeyA, "hold", true, t0);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        release_at(&state, &tx, "hold", first_release);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));

        // Settings changed A -> B inside the double-tap window. This is a new
        // binding, not the second half of A's gesture.
        press_at(
            &state,
            &tx,
            Key::KeyB,
            "hold",
            true,
            first_release + Duration::from_millis(20),
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        let st = state.lock();
        assert!(!st.latched);
        assert!(st.tap_candidate.is_none());
        assert_eq!(st.pressed_key, Some(Key::KeyB));
        drop(st);
        assert_no_cmd(&rx);
    }

    #[test]
    fn out_of_order_clock_input_cannot_revive_a_stale_tap() {
        let (state, tx, rx) = mk();
        let t0 = Instant::now();
        {
            state.lock().tap_candidate = Some(TapCandidate {
                key: Key::KeyA,
                released_at: t0 + Duration::from_secs(1),
            });
        }

        // `checked_duration_since` makes even a synthetic backwards timestamp
        // a plain Start rather than a panic or a false latch.
        press_at(&state, &tx, Key::KeyA, "hold", true, t0);
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        assert!(!state.lock().latched);
        assert_no_cmd(&rx);
    }

    #[test]
    fn zero_interval_double_tap_and_unlatch_have_no_deferred_actions() {
        let (state, tx, rx) = mk();
        let now = Instant::now();

        press_at(&state, &tx, Key::KeyA, "hold", true, now);
        release_at(&state, &tx, "hold", now);
        press_at(&state, &tx, Key::KeyA, "hold", true, now);
        release_at(&state, &tx, "hold", now);
        press_at(&state, &tx, Key::KeyA, "hold", true, now);
        release_at(&state, &tx, "hold", now);

        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StartLatched)));
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        assert_no_cmd(&rx);
        let st = state.lock();
        assert!(!st.key_down);
        assert!(!st.latched);
        assert!(st.pressed_key.is_none());
        assert!(st.tap_candidate.is_none());
    }

    #[test]
    fn duplicate_down_and_up_do_not_break_a_double_tap_candidate() {
        let (state, tx, rx) = mk();
        let t0 = Instant::now();
        let release = t0 + Duration::from_millis(20);

        press_at(&state, &tx, Key::KeyA, "hold", true, t0);
        press_at(&state, &tx, Key::KeyA, "hold", true, t0); // auto-repeat
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        assert_no_cmd(&rx);

        release_at(&state, &tx, "hold", release);
        release_at(&state, &tx, "hold", release); // duplicate/orphan up
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));
        assert_no_cmd(&rx);
        assert_eq!(
            state.lock().tap_candidate.map(|candidate| candidate.key),
            Some(Key::KeyA)
        );

        press_at(
            &state,
            &tx,
            Key::KeyA,
            "hold",
            true,
            release + Duration::from_millis(20),
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StartLatched)));
        assert_no_cmd(&rx);
    }

    // Двойной тап → первый release немедленно StopTap, второй press
    // даёт единую StartLatched; release в защёлке игнорируется.
    #[test]
    fn double_tap_latch_then_press_unlatches() {
        let (state, tx, rx) = mk();
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));
        // Второй tap внутри окна снова запускает запись и защёлкивает её.
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StartLatched)));
        assert_no_cmd(&rx);
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(state.lock().latched);
        assert_no_cmd(&rx);
        // Нажатие в защёлке — выключение.
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
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
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StopTap)));
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::StartLatched)));
        assert_no_cmd(&rx);
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(state.lock().latched);
        // Выключающее нажатие A; target меняется на B ДО release.
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyA),
            Key::KeyA,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Stop)));
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyRelease(Key::KeyA),
            Key::KeyB,
            "hold",
            true,
        );
        {
            let st = state.lock();
            assert!(!st.key_down && !st.ignore_release && st.pressed_key.is_none());
        }
        assert_no_cmd(&rx);
        // Новый хоткей работает с первого раза.
        dispatch_with_latch(
            &state,
            &tx,
            EventType::KeyPress(Key::KeyB),
            Key::KeyB,
            "hold",
            true,
        );
        assert!(matches!(rx.try_recv(), Ok(EngineCmd::Start)));
    }
}
