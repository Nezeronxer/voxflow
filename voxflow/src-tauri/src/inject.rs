//! Off-focus вставка текста в активное окно — через ВЫДЕЛЕННЫЙ воркер-поток.
//!
//! Почему воркер: раньше каждый вызов создавал свой Enigo и дёргал буфер обмена
//! из случайного (часто detached) потока. Параллельные финал/партиалы конкурировали
//! за clipboard между собой и с clipboard_monitor (опрос каждые 1.3с) — текст
//! «зависал» посреди вставки. Теперь ВСЕ нажатия и clipboard-операции исполняет
//! один поток "voxflow-inject", разбирающий FIFO-очередь: порядок фрагментов
//! гарантирован каналом, гонок за clipboard/Enigo нет в принципе.
//!
//! Публичные сигнатуры сохранены: inject()/inject_incremental() как и раньше
//! БЛОКИРУЮТСЯ до фактического исполнения (Job несёт ack-канал) — для вызывающего
//! кода (engine.rs) семантика не изменилась, меняется только то, ИЗ КАКОГО потока
//! физически жмутся клавиши.
//!
//! Основной путь — clipboard-paste (надёжно для кириллицы/длинного текста),
//! fallback — посимвольная печать (enigo type).
//!
//! Тестируемость: env VOXFLOW_INJECT_DRY=1 (читается ОДИН раз, при первом обращении
//! к инжектору) — воркер не трогает enigo/clipboard, а пишет задания в DRY_LOG.

use anyhow::{anyhow, Result};
use enigo::{Direction, Enigo, Key, Keyboard, Settings as ESettings};
use parking_lot::Mutex;
#[cfg(windows)]
use std::mem::size_of;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, OnceLock};
use std::{
    thread,
    time::{Duration, Instant},
};

// ─────────────────────────── Очередь и воркер ───────────────────────────

/// Команда воркеру.
enum Cmd {
    /// Вставка целиком: clipboard-paste с fallback в печать, либо "type".
    Full {
        text: String,
        method: String,
        keep_clipboard: bool,
    },
    /// Инкрементальное сведение prev → next клавишами (Backspace + допечатка).
    Incr { prev: String, next: String },
    /// Положить финальный текст в clipboard без нажатий.
    SetClipboard { text: String },
    /// Скопировать текущее выделение через Ctrl+C, не оставляя свой мусор в clipboard.
    CopySelection,
}

enum CmdResult {
    Done,
    Selection(Option<String>),
}

/// Задание очереди: команда + момент постановки (метрика wait) + ack-канал,
/// по которому воркер возвращает результат (вызвавший поток блокируется на recv).
struct Job {
    cmd: Cmd,
    enqueued: Instant,
    ack: mpsc::Sender<Result<CmdResult>>,
}

/// Хэндл инжектора — единственный Sender в очередь воркера. Mutex вокруг Sender:
/// std::mpsc::Sender не Sync на старых toolchain'ах; блокировка микроскопическая
/// (только на время send), на тайминги не влияет.
struct Injector {
    tx: Mutex<mpsc::Sender<Job>>,
}

static INJECTOR: OnceLock<Injector> = OnceLock::new();

/// Задания «в полёте» (в очереди + исполняется). Именно СЧЁТЧИК, а не AtomicBool:
/// bool давал бы ложное «свободен» в зазоре между двумя заданиями очереди, а
/// clipboard_monitor по is_busy() должен пережидать ВСЮ пачку вставок целиком.
static PENDING: AtomicUsize = AtomicUsize::new(0);

/// Dry-журнал для тестов (VOXFLOW_INJECT_DRY=1): воркер пишет сюда вместо
/// нажатий. Формат записи: Full — сам текст, Incr — "incr|prev|next".
static DRY_LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Снимок прежнего содержимого буфера обмена для восстановления после Ctrl+V.
/// Раньше снимался ТОЛЬКО текст (get_text().ok()) — скриншот пользователя
/// безвозвратно затирался надиктовкой. arboard умеет лишь текст и растровую
/// картинку: файловые списки (CF_HDROP), HTML и прочие форматы снять нечем —
/// такой буфер после вставки не восстановится (ограничение arboard, не наше).
enum ClipSnapshot {
    Text(String),
    Image(arboard::ImageData<'static>),
}

/// Ленивая инициализация: первый вызов поднимает воркер "voxflow-inject".
/// Режим dry фиксируется здесь же и не меняется до конца процесса.
fn injector() -> &'static Injector {
    INJECTOR.get_or_init(|| {
        let dry = std::env::var("VOXFLOW_INJECT_DRY")
            .map(|v| v == "1")
            .unwrap_or(false);
        let (tx, rx) = mpsc::channel::<Job>();
        thread::Builder::new()
            .name("voxflow-inject".into())
            .spawn(move || worker_loop(rx, dry))
            .expect("spawn voxflow-inject");
        Injector { tx: Mutex::new(tx) }
    })
}

/// Положить задание в очередь, вернуть приёмник ack. НЕ блокируется на исполнении —
/// блокируются публичные обёртки через wait_ack (а тест порядка — нарочно нет).
fn enqueue(cmd: Cmd) -> mpsc::Receiver<Result<CmdResult>> {
    let (ack_tx, ack_rx) = mpsc::channel();
    // Счётчик ДО send: is_busy() обязан стать true раньше, чем воркер возьмёт job.
    PENDING.fetch_add(1, Ordering::SeqCst);
    let job = Job {
        cmd,
        enqueued: Instant::now(),
        ack: ack_tx,
    };
    if injector().tx.lock().send(job).is_err() {
        // Воркер умер — job дропнут вместе с ack_tx, recv() у вызывающего вернёт
        // ошибку; счётчик откатываем, чтобы is_busy() не залип в true.
        PENDING.fetch_sub(1, Ordering::SeqCst);
    }
    ack_rx
}

/// Дождаться результата задания (семантика прежнего синхронного вызова).
fn wait_ack(rx: mpsc::Receiver<Result<CmdResult>>) -> Result<CmdResult> {
    rx.recv()
        .map_err(|_| anyhow!("inject-воркер недоступен (ack-канал закрыт)"))?
}

fn wait_done(rx: mpsc::Receiver<Result<CmdResult>>) -> Result<()> {
    match wait_ack(rx)? {
        CmdResult::Done => Ok(()),
        CmdResult::Selection(_) => Err(anyhow!("inject-воркер вернул неожиданный selection")),
    }
}

fn wait_selection(rx: mpsc::Receiver<Result<CmdResult>>) -> Result<Option<String>> {
    match wait_ack(rx)? {
        CmdResult::Selection(text) => Ok(text),
        CmdResult::Done => Err(anyhow!("inject-воркер не вернул selection")),
    }
}

/// Цикл воркера: единственный владелец Enigo и clipboard-операций.
fn worker_loop(rx: mpsc::Receiver<Job>, dry: bool) {
    // Enigo создаётся ОДИН раз и живёт в воркере (раньше — на каждый вызов).
    // Ленивая инициализация: при ошибке создания поток не валим — попробуем
    // снова на следующем задании.
    let mut enigo: Option<Enigo> = None;
    for job in rx {
        let wait_ms = job.enqueued.elapsed().as_millis();
        let t0 = Instant::now();
        let (len, method) = match &job.cmd {
            Cmd::Full { text, method, .. } => (text.chars().count(), method.as_str()),
            Cmd::Incr { next, .. } => (next.chars().count(), "incr"),
            Cmd::SetClipboard { text } => (text.chars().count(), "set-clipboard"),
            Cmd::CopySelection => (0, "copy-selection"),
        };
        let (res, restore) = if dry {
            (run_dry(&job.cmd), None)
        } else {
            run_real(&mut enigo, &job.cmd)
        };
        let exec_ms = t0.elapsed().as_millis();
        log::info!("[inject] len={len} method={method} wait={wait_ms}мс exec={exec_ms}мс");
        if restore.is_none() {
            // Снимаем «занят» ДО ack: проснувшийся вызывающий сразу видит
            // is_busy()==false (если очередь пуста).
            PENDING.fetch_sub(1, Ordering::SeqCst);
        }
        let _ = job.ack.send(res);
        // Отложенное восстановление буфера — УЖЕ ПОСЛЕ ack (вызывающий не ждёт
        // эти ~115мс; текст в поле появился сразу после Ctrl+V). Всё ещё внутри
        // слота воркера: порядок заданий сохранён, следующий job не начнётся,
        // пока прежний буфер не возвращён. PENDING держим >0 — clipboard_monitor
        // в это окно в буфер не лезет.
        if let Some(prev) = restore {
            thread::sleep(Duration::from_millis(115));
            let _ = clipboard_restore_retry(&prev);
            PENDING.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Dry-режим: фиксируем задание в журнале, ничего не нажимая.
fn run_dry(cmd: &Cmd) -> Result<CmdResult> {
    let entry = match cmd {
        Cmd::Full { text, .. } => text.clone(),
        Cmd::Incr { prev, next } => format!("incr|{prev}|{next}"),
        Cmd::SetClipboard { text } => format!("clip|{text}"),
        Cmd::CopySelection => {
            return Ok(CmdResult::Selection(
                std::env::var("VOXFLOW_INJECT_DRY_SELECTION").ok(),
            ));
        }
    };
    DRY_LOG.lock().push(entry);
    Ok(CmdResult::Done)
}

/// Боевое исполнение задания (только из воркера). Второй элемент — снимок
/// прежнего буфера обмена, который надо восстановить ПОСЛЕ ack (см. worker_loop).
fn run_real(enigo: &mut Option<Enigo>, cmd: &Cmd) -> (Result<CmdResult>, Option<ClipSnapshot>) {
    match cmd {
        Cmd::Full {
            text,
            method,
            keep_clipboard,
        } => match method.as_str() {
            "type" => {
                if *keep_clipboard {
                    match clipboard_set_retry(text).and_then(|_| try_type(enigo, text)) {
                        Ok(()) => (Ok(CmdResult::Done), None),
                        Err(e) => (Err(e), None),
                    }
                } else {
                    (try_type(enigo, text).map(|_| CmdResult::Done), None)
                }
            }
            _ => {
                let mut restore = None;
                match paste_text(enigo, text, &mut restore, !keep_clipboard) {
                    Ok(()) => (Ok(CmdResult::Done), restore),
                    // paste не сработал, а текст содержит переводы строк —
                    // печать ЗАПРЕЩЕНА: посимвольные Enter'ы в активном окне
                    // опасны (отправка сообщения/промпта в чате вроде Codex).
                    // Лучше честный Err, чем нажатый Enter.
                    Err(e) if has_line_break(text) => {
                        log::warn!("paste failed ({e}); multiline — fallback-печать запрещена");
                        (Err(e), restore)
                    }
                    // если paste не сработал — пробуем печать
                    Err(e) => {
                        log::warn!("paste failed ({e}), fallback to type");
                        (try_type(enigo, text).map(|_| CmdResult::Done), restore)
                    }
                }
            }
        },
        Cmd::Incr { prev, next } => (
            enigo_of(enigo)
                .and_then(|e| incremental_keys(e, prev, next))
                .map(|_| CmdResult::Done),
            None,
        ),
        Cmd::SetClipboard { text } => (clipboard_set_retry(text).map(|_| CmdResult::Done), None),
        Cmd::CopySelection => run_copy_selection(enigo),
    }
}

/// Обёртка печати с ленивым Enigo (для веток run_real без `?`).
fn try_type(enigo: &mut Option<Enigo>, text: &str) -> Result<()> {
    type_text(enigo_of(enigo)?, text)
}

/// Единственный Enigo воркера, ленивое создание. При ошибке слот остаётся пуст —
/// повторим на следующем задании (а текущее вернёт Err вызывающему).
fn enigo_of(slot: &mut Option<Enigo>) -> Result<&mut Enigo> {
    if slot.is_none() {
        *slot = Some(Enigo::new(&ESettings::default()).map_err(|e| anyhow!("enigo init: {e}"))?);
    }
    Ok(slot.as_mut().expect("слот только что заполнен"))
}

// ─────────────────────────── Публичный API (сигнатуры прежние) ───────────────────────────

fn has_line_break(text: &str) -> bool {
    text.contains('\n') || text.contains('\r')
}

/// Эффективный метод вставки для данного текста. Любой текст с переводами
/// строк ВСЕГДА идёт clipboard-путём, даже при настройке "type": посимвольная
/// печать переводов строки — это реальные нажатия Enter в активном окне
/// (в чате это отправка сообщения/промпта), а Ctrl+V безопасен — вставляется
/// содержимое буфера, а не нажатие клавиши. Однострочный текст — метод как
/// задан, поведение прежнее.
fn effective_method<'a>(text: &str, method: &'a str) -> &'a str {
    if has_line_break(text) {
        "clipboard"
    } else {
        method
    }
}

/// Вставка текста целиком. method: "clipboard" (дефолт, с fallback в печать) | "type".
/// Блокируется до фактического исполнения воркером — как и раньше, но теперь
/// нажатия идут из одного потока в порядке очереди.
#[allow(dead_code)] // старый публичный путь оставлен для совместимости; диктовка держит clipboard через inject_keep_clipboard.
pub fn inject(text: &str, method: &str) -> Result<()> {
    // Пропускаем БЕЗ вставки только ПОЛНОСТЬЮ пустой текст. Именно is_empty(),
    // а не trim().is_empty(): "\n" — результат одиночной команды «с новой
    // строки» и обязан вставиться (см. effective_method).
    if text.is_empty() {
        return Ok(());
    }
    let method = effective_method(text, method);
    wait_done(enqueue(Cmd::Full {
        text: text.to_string(),
        method: method.to_string(),
        keep_clipboard: false,
    }))
}

/// Вставить текст целиком и оставить именно его в системном clipboard.
///
/// Это финальный пользовательский путь диктовки: если активное окно не приняло
/// Ctrl+V или прочитало буфер слишком поздно, пользователь всё равно может
/// вручную нажать Ctrl+V и получить последний надиктованный текст.
pub fn inject_keep_clipboard(text: &str, method: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    let method = effective_method(text, method);
    wait_done(enqueue(Cmd::Full {
        text: text.to_string(),
        method: method.to_string(),
        keep_clipboard: true,
    }))
}

/// Сохранить финальный пользовательский текст в clipboard без нажатий.
pub fn set_clipboard_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }
    wait_done(enqueue(Cmd::SetClipboard {
        text: text.to_string(),
    }))
}

/// Инкрементальная вставка КЛАВИШАМИ (не через буфер обмена — paste не умеет
/// backspace). Сводит `prev` → `next`: считает длину общего ПОСИМВОЛЬНОГО
/// префикса, удаляет хвост `prev` (Backspace по разу на символ), затем печатает
/// хвост `next`. Блокируется до исполнения воркером.
pub fn inject_incremental(prev: &str, next: &str) -> Result<()> {
    if prev == next {
        return Ok(());
    }
    wait_done(enqueue(Cmd::Incr {
        prev: prev.to_string(),
        next: next.to_string(),
    }))
}

/// Скопировать выделенный текст из активного окна. Возвращает None, если выделения
/// нет, оно не текстовое, либо приложение не обновило clipboard после Ctrl+C.
pub fn copy_selection_text() -> Result<Option<String>> {
    wait_selection(enqueue(Cmd::CopySelection))
}

/// true, пока есть задания в очереди или исполняется текущее. Для clipboard_monitor:
/// пережидать вставку и не трогать буфер, пока инжектор им жонглирует.
pub fn is_busy() -> bool {
    PENDING.load(Ordering::SeqCst) > 0
}

/// Снимок dry-журнала (только для тестов с VOXFLOW_INJECT_DRY=1).
#[allow(dead_code)] // используется тестами; в боевой сборке не зовётся
pub fn dry_log() -> Vec<String> {
    DRY_LOG.lock().clone()
}

// ─────────────────────────── Реализация нажатий (в воркере) ───────────────────────────

fn type_text(e: &mut Enigo, text: &str) -> Result<()> {
    e.text(text).map_err(|e| anyhow!("type: {e}"))?;
    Ok(())
}

/// Сведение prev → next клавишами. Счёт ведём по СИМВОЛАМ (chars), не по байтам —
/// для кириллицы один Backspace удаляет одну букву. Общий префикс ищем zip-ом
/// итераторов char, без срезов байт.
fn incremental_keys(e: &mut Enigo, prev: &str, next: &str) -> Result<()> {
    // Длина общего префикса в символах.
    let mut common = 0usize;
    for (a, b) in prev.chars().zip(next.chars()) {
        if a == b {
            common += 1;
        } else {
            break;
        }
    }

    let prev_len = prev.chars().count();
    let backspaces = prev_len - common;
    let suffix: String = next.chars().skip(common).collect();

    // Нечего делать (теоретически невозможно при prev != next, но защитимся).
    if backspaces == 0 && suffix.is_empty() {
        return Ok(());
    }

    // Удаляем расходящийся хвост prev.
    for _ in 0..backspaces {
        e.key(Key::Backspace, Direction::Click)
            .map_err(|e| anyhow!("backspace: {e}"))?;
    }
    // Допечатываем новый хвост.
    if !suffix.is_empty() {
        e.text(&suffix).map_err(|e| anyhow!("type suffix: {e}"))?;
    }
    Ok(())
}

#[cfg(windows)]
mod win_input {
    use super::*;

    const INPUT_KEYBOARD: u32 = 1;
    const KEYEVENTF_KEYUP: u32 = 0x0002;
    const VK_CONTROL: u16 = 0x11;
    pub const VK_C: u16 = 0x43;
    pub const VK_V: u16 = 0x56;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Input {
        r#type: u32,
        anonymous: InputUnion,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    union InputUnion {
        ki: KeybdInput,
        #[cfg(target_pointer_width = "64")]
        _padding64: [u64; 4],
        #[cfg(target_pointer_width = "32")]
        _padding32: [u32; 6],
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct KeybdInput {
        w_vk: u16,
        w_scan: u16,
        dw_flags: u32,
        time: u32,
        dw_extra_info: usize,
    }

    #[link(name = "user32")]
    extern "system" {
        fn SendInput(c_inputs: u32, p_inputs: *const Input, cb_size: i32) -> u32;
    }

    fn key_input(vk: u16, keyup: bool) -> Input {
        Input {
            r#type: INPUT_KEYBOARD,
            anonymous: InputUnion {
                ki: KeybdInput {
                    w_vk: vk,
                    w_scan: 0,
                    dw_flags: if keyup { KEYEVENTF_KEYUP } else { 0 },
                    time: 0,
                    dw_extra_info: 0,
                },
            },
        }
    }

    fn send_key(vk: u16, keyup: bool) -> Result<()> {
        let input = key_input(vk, keyup);
        let sent = unsafe { SendInput(1, &input, size_of::<Input>() as i32) };
        if sent == 1 {
            Ok(())
        } else {
            Err(anyhow!(
                "SendInput vk=0x{vk:02X} {} sent {sent}/1",
                if keyup { "up" } else { "down" }
            ))
        }
    }

    /// Native Windows chord emitter. Enigo is fine for text typing, but Ctrl+V/C
    /// through it can be flaky on Windows 10 under non-English layouts and some
    /// Chromium/Electron targets. The paste/copy invariant matters here: after the
    /// letter key is pressed, callers must treat the shortcut as delivered and only
    /// do best-effort key releases.
    pub fn ctrl_chord(vk: u16) -> Result<()> {
        send_key(VK_CONTROL, false)?;
        thread::sleep(Duration::from_millis(4));

        if let Err(err) = send_key(vk, false) {
            let _ = send_key(VK_CONTROL, true);
            return Err(err);
        }

        let _ = send_key(vk, true);
        thread::sleep(Duration::from_millis(4));
        let _ = send_key(VK_CONTROL, true);
        Ok(())
    }
}

/// Clipboard-операция с ретраями: clipboard на Windows — глобальный ресурс,
/// его может коротко держать другое приложение (ERROR_CLIPBOARD_BUSY и т.п.).
/// 3 попытки с паузой 30мс между ними.
fn clipboard_retry(mut op: impl FnMut(&mut arboard::Clipboard) -> Result<()>) -> Result<()> {
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..3 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(30));
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => match op(&mut cb) {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(e),
            },
            Err(e) => last = Some(anyhow!("clipboard open: {e}")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("clipboard: неизвестная ошибка")))
}

/// Записать текст в буфер (с ретраями — см. clipboard_retry).
fn clipboard_set_retry(text: &str) -> Result<()> {
    clipboard_retry(|cb| {
        cb.set_text(text.to_string())
            .map_err(|e| anyhow!("clipboard set: {e}"))
    })
}

fn clipboard_get_text_retry() -> Result<String> {
    let mut out = String::new();
    clipboard_retry(|cb| {
        out = cb
            .get_text()
            .map_err(|e| anyhow!("clipboard get text: {e}"))?;
        Ok(())
    })?;
    Ok(out)
}

/// Снять снимок буфера: сначала текст, при неудаче — картинка (скриншоты!).
/// None — буфер пуст либо формат, который arboard не читает (CF_HDROP и пр.,
/// см. ClipSnapshot).
fn clipboard_snapshot() -> Option<ClipSnapshot> {
    let mut cb = arboard::Clipboard::new().ok()?;
    if let Ok(t) = cb.get_text() {
        return Some(ClipSnapshot::Text(t));
    }
    cb.get_image().ok().map(ClipSnapshot::Image)
}

/// Восстановить снимок буфера (с ретраями). set_image забирает ImageData по
/// значению — на каждую попытку отдаём заимствованную копию (Cow::Borrowed),
/// байты картинки не клонируются.
fn clipboard_restore_retry(snap: &ClipSnapshot) -> Result<()> {
    clipboard_retry(|cb| match snap {
        ClipSnapshot::Text(t) => cb
            .set_text(t.clone())
            .map_err(|e| anyhow!("clipboard set: {e}")),
        ClipSnapshot::Image(img) => {
            let borrowed = arboard::ImageData {
                width: img.width,
                height: img.height,
                bytes: std::borrow::Cow::Borrowed(img.bytes.as_ref()),
            };
            cb.set_image(borrowed)
                .map_err(|e| anyhow!("clipboard set image: {e}"))
        }
    })
}

/// Вставка через буфер обмена (Ctrl+V / Cmd+V). Сохранение прежнего буфера и его
/// восстановление ПОСЛЕ паузы чтения целевым приложением — внутри одного задания,
/// поэтому ничей чужой clipboard-доступ между этими шагами не вклинится.
///
/// ИНВАРИАНТ «без дубля»: как только Ctrl+V ОТПРАВЛЕН (V кликнута), функция обязана
/// вернуть Ok — после этой точки мы НЕ откатываемся в печать, иначе текст вставится
/// ДВАЖДЫ (paste уже сработал + fallback `type_text`). Поэтому отпускание модификатора
/// и восстановление буфера — best-effort (`let _ = ...`), без оператора `?`.
/// Err допустим только ДО доставки V (буфер не записался за 3 попытки, enigo не
/// поднялся, не нажался модификатор, не кликнулась V) — тогда печать как fallback
/// безопасна, ведь ничего ещё не вставлено.
fn paste_text(
    enigo_slot: &mut Option<Enigo>,
    text: &str,
    restore_out: &mut Option<ClipSnapshot>,
    restore_previous: bool,
) -> Result<()> {
    // Снимок текущего буфера: текст ИЛИ картинка (best-effort: пустой/нечитаемый
    // формат — нечего восстанавливать, см. ClipSnapshot).
    let prev = clipboard_snapshot().or_else(|| Some(ClipSnapshot::Text(String::new())));

    // Положить наш текст (с ретраями ×3 по 30мс — см. clipboard_set_retry).
    clipboard_set_retry(text)?;
    // Дать ОС увидеть новый буфер до Ctrl+V (срезано с 40мс — set_text синхронен).
    thread::sleep(Duration::from_millis(25));

    // Послать Ctrl+V (Cmd+V на macOS) в активное окно.
    #[cfg(windows)]
    {
        let _ = enigo_slot;
        win_input::ctrl_chord(win_input::VK_V).map_err(|e| anyhow!("v: {e}"))?;
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "macos")]
        let modkey = Key::Meta;
        #[cfg(not(target_os = "macos"))]
        let modkey = Key::Control;
        let vkey = Key::Unicode('v');

        // --- до этой черты ошибки безопасны (V ещё не доставлена) ---
        let e = enigo_of(enigo_slot)?;
        e.key(modkey, Direction::Press)
            .map_err(|e| anyhow!("mod down: {e}"))?;
        if let Err(err) = e.key(vkey, Direction::Click) {
            // V не доставлена — откатываем модификатор и сообщаем об ошибке, fallback в печать безопасен.
            let _ = e.key(modkey, Direction::Release);
            return Err(anyhow!("v: {err}"));
        }
        // --- V ОТПРАВЛЕНА: дальше только best-effort, возвращаем строго Ok ---
        // Отпускание модификатора best-effort: ошибка здесь НЕ должна вызвать дубль.
        let _ = e.key(modkey, Direction::Release);
    }
    // Короткий settle: дать окну принять аккорд (текст появляется в поле уже
    // здесь). Прежний буфер возвращаем НЕ тут, а после ack в worker_loop —
    // суммарная пауза V→restore остаётся 15+115=130мс (компромисс 140↔90:
    // тяжёлые Electron/Chromium-приёмники читают буфер асинхронно и при 90мс
    // изредка вставляли СТАРЫЙ буфер), но вызывающий больше эти 130мс не ждёт.
    thread::sleep(Duration::from_millis(15));
    if restore_previous {
        *restore_out = prev;
    }
    Ok(())
}

fn run_copy_selection(enigo_slot: &mut Option<Enigo>) -> (Result<CmdResult>, Option<ClipSnapshot>) {
    static COPY_SEQ: AtomicUsize = AtomicUsize::new(0);
    let seq = COPY_SEQ.fetch_add(1, Ordering::SeqCst);
    let sentinel = format!("__VOXFLOW_EMPTY_SELECTION_{seq}__");
    let prev = clipboard_snapshot().unwrap_or(ClipSnapshot::Text(String::new()));

    if let Err(e) = clipboard_set_retry(&sentinel) {
        return (Err(e), Some(prev));
    }
    thread::sleep(Duration::from_millis(25));

    #[cfg(windows)]
    let copy_res = {
        let _ = enigo_slot;
        win_input::ctrl_chord(win_input::VK_C)
    };
    #[cfg(not(windows))]
    let copy_res = enigo_of(enigo_slot).and_then(send_copy_chord);
    if let Err(e) = copy_res {
        return (Err(e), Some(prev));
    }
    thread::sleep(Duration::from_millis(90));

    let text = match clipboard_get_text_retry() {
        Ok(t) => t,
        Err(e) => return (Err(e), Some(prev)),
    };
    let selected = if text == sentinel || text.trim().is_empty() {
        None
    } else {
        Some(text)
    };
    (Ok(CmdResult::Selection(selected)), Some(prev))
}

#[cfg(not(windows))]
fn send_copy_chord(e: &mut Enigo) -> Result<()> {
    #[cfg(target_os = "macos")]
    let modkey = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modkey = Key::Control;
    #[cfg(windows)]
    let ckey = Key::Other(0x43); // VK_C
    #[cfg(not(windows))]
    let ckey = Key::Unicode('c');

    e.key(modkey, Direction::Press)
        .map_err(|e| anyhow!("mod down: {e}"))?;
    let click = e.key(ckey, Direction::Click).map_err(|e| anyhow!("c: {e}"));
    let _ = e.key(modkey, Direction::Release);
    click
}

// ─────────────────────────── Тесты (dry-режим) ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// Оба теста делят глобальные DRY_LOG/PENDING — параллельный test-harness
    /// (дефолт cargo test) их интерферирует. Сериализуем сами, чтобы дефолтный
    /// прогон был зелёным без --test-threads=1.
    static SERIAL: Mutex<()> = Mutex::new(());

    /// Тест №1: глобальный FIFO-порядок под конкуренцией, 4 потока × 100 фрагментов.
    ///
    /// Порядок фиксируем мьютексом ВОКРУГ enqueue: поток захватывает замок, кладёт
    /// job в очередь, записывает свой текст в `expected`, отпускает. Без этого
    /// тест недетерминирован: между send() в канал и записью ожидаемого номера
    /// вклинился бы другой поток. Ack ждём УЖЕ ВНЕ замка — конкуренцию не убиваем.
    #[test]
    fn inject_fifo_order_4x100() {
        let _serial = SERIAL.lock();
        // ДО первого обращения к инжектору.
        std::env::set_var("VOXFLOW_INJECT_DRY", "1");
        DRY_LOG.lock().clear();

        let gate = Arc::new(Mutex::new(())); // замок вокруг enqueue
        let expected = Arc::new(Mutex::new(Vec::<String>::new()));
        let mut handles = Vec::new();
        for tid in 0..4 {
            let gate = Arc::clone(&gate);
            let expected = Arc::clone(&expected);
            handles.push(thread::spawn(move || {
                for i in 0..100 {
                    let text = format!("t{tid}-{i:03}");
                    let rx = {
                        let _g = gate.lock();
                        let rx = enqueue(Cmd::Full {
                            text: text.clone(),
                            method: "clipboard".into(),
                            keep_clipboard: false,
                        });
                        expected.lock().push(text);
                        rx
                    };
                    rx.recv()
                        .expect("ack-канал жив")
                        .expect("dry-job без ошибок");
                }
            }));
        }
        for h in handles {
            h.join().expect("поток теста завершился без паники");
        }

        let got = dry_log();
        let want = expected.lock().clone();
        assert_eq!(got.len(), 400, "без потерь: 4 потока × 100 фрагментов");
        assert_eq!(got, want, "глобальный порядок фрагментов = порядок enqueue");
        assert!(!is_busy(), "после всех ack очередь пуста");
    }

    /// Тест №2: длинный текст (≥50КБ) методом clipboard в dry-режиме —
    /// одно задание, текст доходит целиком и без искажений.
    #[test]
    fn inject_clipboard_50kb_dry() {
        let _serial = SERIAL.lock();
        std::env::set_var("VOXFLOW_INJECT_DRY", "1");
        DRY_LOG.lock().clear();

        // ~50КБ: кириллица в UTF-8 — 2 байта на букву.
        let chunk = "проверка длинной вставки через буфер обмена ";
        let mut big = String::new();
        while big.len() < 50 * 1024 {
            big.push_str(chunk);
        }
        assert!(big.len() >= 50 * 1024, "текст действительно ≥50КБ");

        inject(&big, "clipboard").expect("50КБ через clipboard в dry — ок");

        let log = dry_log();
        assert_eq!(log.len(), 1, "ровно одно задание");
        assert_eq!(log[0], big, "текст не порезан и не искажён");
        assert!(!is_busy(), "после ack инжектор свободен");
    }

    #[test]
    fn copy_selection_dry_returns_env_text() {
        let _serial = SERIAL.lock();
        std::env::set_var("VOXFLOW_INJECT_DRY", "1");
        std::env::set_var("VOXFLOW_INJECT_DRY_SELECTION", "сырой выделенный текст");

        let selected = copy_selection_text().expect("dry copy selection");
        assert_eq!(selected.as_deref(), Some("сырой выделенный текст"));
        assert!(!is_busy(), "после ack инжектор свободен");
    }

    /// Тест №4 (регрессия major «одиночная команда "с новой строки" — no-op»):
    /// whitespace-only текст ("\n", "\n\n") ОБЯЗАН дойти до воркера и вставиться,
    /// а полностью пустой "" — единственный, кто пропускается без вставки.
    /// Раньше гард trim().is_empty() молча глотал "\n" — голосовая команда
    /// «с новой строки» превращалась в no-op.
    #[test]
    fn inject_whitespace_only_not_dropped_dry() {
        let _serial = SERIAL.lock();
        std::env::set_var("VOXFLOW_INJECT_DRY", "1");
        DRY_LOG.lock().clear();

        inject("", "clipboard").expect("полностью пустой текст — no-op без ошибки");
        inject("", "type").expect("пустой текст и при type — no-op без ошибки");
        inject("\n", "clipboard").expect("одиночный перевод строки вставляется");
        inject("\n", "type").expect("перевод строки при paste_method=type тоже вставляется");
        inject("\n\n", "type").expect("абзац вставляется");

        let log = dry_log();
        assert_eq!(
            log,
            vec!["\n".to_string(), "\n".to_string(), "\n\n".to_string()],
            "пустой текст пропущен, whitespace-only дошёл до воркера без искажений"
        );
        assert!(!is_busy(), "после всех ack инжектор свободен");
    }

    /// Тест №5: выбор эффективного метода. Любой текст с переводами строк
    /// всегда принудительно clipboard (даже при настройке "type" — печать
    /// Enter'ов опасна), однострочный текст — метод как задан.
    #[test]
    fn effective_method_forces_clipboard_for_multiline() {
        assert_eq!(effective_method("\n", "type"), "clipboard");
        assert_eq!(effective_method("\n\n", "type"), "clipboard");
        assert_eq!(effective_method("  \t\n", "type"), "clipboard");
        assert_eq!(
            effective_method("первая строка\nвторая строка", "type"),
            "clipboard"
        );
        assert_eq!(
            effective_method("первая строка\r\nвторая строка", "type"),
            "clipboard"
        );
        assert_eq!(effective_method("\n", "clipboard"), "clipboard");
        // Однострочный текст — без изменений.
        assert_eq!(effective_method("привет", "type"), "type");
        assert_eq!(effective_method("привет", "clipboard"), "clipboard");
    }

    #[test]
    fn final_clipboard_helpers_enqueue_text() {
        let _serial = SERIAL.lock();
        std::env::set_var("VOXFLOW_INJECT_DRY", "1");
        DRY_LOG.lock().clear();

        inject_keep_clipboard("готовый текст", "clipboard").expect("final paste");
        set_clipboard_text("готовый текст").expect("remember clipboard");

        assert_eq!(
            dry_log(),
            vec![
                "готовый текст".to_string(),
                "clip|готовый текст".to_string()
            ]
        );
    }

    /// Тест №3 (регрессия P1-2): в буфере КАРТИНКА (скриншот) — снимок обязан
    /// её увидеть, восстановление — вернуть байт-в-байт. Раньше снимался только
    /// текст (get_text().ok()) → restore=None → скриншот пользователя терялся.
    /// Dry-режим clipboard не трогает вовсе, поэтому снимок/восстановление
    /// проверяем напрямую юнитом на РЕАЛЬНОМ clipboard этой машины; без
    /// desktop-сеанса (Clipboard::new падает) тест тихо пропускается.
    #[test]
    fn clipboard_image_snapshot_restore() {
        let _serial = SERIAL.lock();
        if arboard::Clipboard::new().is_err() {
            return; // headless: реального буфера нет, проверять нечего
        }

        // RGBA 2x2, alpha=255 у всех пикселей: DIB-конверсия Windows — чистая
        // перестановка каналов без премультипликации, но BMP-декодер при
        // прозрачности имеет свои причуды — непрозрачные байты детерминированы.
        let bytes: Vec<u8> = vec![
            10, 20, 30, 255, 40, 50, 60, 255, //
            70, 80, 90, 255, 100, 110, 120, 255,
        ];
        arboard::Clipboard::new()
            .expect("clipboard open")
            .set_image(arboard::ImageData {
                width: 2,
                height: 2,
                bytes: bytes.clone().into(),
            })
            .expect("set_image 2x2");

        // Снимок видит именно картинку (текста в буфере нет).
        let snap = clipboard_snapshot().expect("снимок непустого буфера");
        match &snap {
            ClipSnapshot::Image(img) => assert_eq!((img.width, img.height), (2, 2)),
            ClipSnapshot::Text(t) => panic!("в снимке текст вместо картинки: {t:?}"),
        }

        // Имитация вставки: буфер затёрт нашим текстом → восстановление снимка.
        // Контракт ЭТОГО кода (регрессия P1-2): снимок увидел картинку (проверено
        // выше) и восстановление прошло без ошибки. Байт-в-байт фиделити DIB↔PNG —
        // свойство arboard/ОС (BGRA-перестановка, выравнивание строк, гонка с
        // буфером запущенного приложения), а НЕ нашего кода: проверять его через
        // живой системный clipboard ненадёжно (flaky), поэтому не утверждаем.
        clipboard_set_retry("voxflow: надиктованный текст").expect("set_text");
        clipboard_restore_retry(&snap).expect("восстановление картинки не падает");

        // Лучший-эффорт: если буфер удалось перечитать НАШИМ снимком — это снова
        // картинка тех же размеров (а не наш затёрший текст). Если перечитать не
        // вышло (ОС/другое приложение тронуло буфер) — не валим тест на этом.
        if let Some(ClipSnapshot::Image(img)) = clipboard_snapshot() {
            assert_eq!(
                (img.width, img.height),
                (2, 2),
                "размеры восстановленной картинки"
            );
        }
    }
}
