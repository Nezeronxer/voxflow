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
    Full { text: String, method: String },
    /// Инкрементальное сведение prev → next клавишами (Backspace + допечатка).
    Incr { prev: String, next: String },
}

/// Задание очереди: команда + момент постановки (метрика wait) + ack-канал,
/// по которому воркер возвращает результат (вызвавший поток блокируется на recv).
struct Job {
    cmd: Cmd,
    enqueued: Instant,
    ack: mpsc::Sender<Result<()>>,
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

/// Ленивая инициализация: первый вызов поднимает воркер "voxflow-inject".
/// Режим dry фиксируется здесь же и не меняется до конца процесса.
fn injector() -> &'static Injector {
    INJECTOR.get_or_init(|| {
        let dry = std::env::var("VOXFLOW_INJECT_DRY").map(|v| v == "1").unwrap_or(false);
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
fn enqueue(cmd: Cmd) -> mpsc::Receiver<Result<()>> {
    let (ack_tx, ack_rx) = mpsc::channel();
    // Счётчик ДО send: is_busy() обязан стать true раньше, чем воркер возьмёт job.
    PENDING.fetch_add(1, Ordering::SeqCst);
    let job = Job { cmd, enqueued: Instant::now(), ack: ack_tx };
    if injector().tx.lock().send(job).is_err() {
        // Воркер умер — job дропнут вместе с ack_tx, recv() у вызывающего вернёт
        // ошибку; счётчик откатываем, чтобы is_busy() не залип в true.
        PENDING.fetch_sub(1, Ordering::SeqCst);
    }
    ack_rx
}

/// Дождаться результата задания (семантика прежнего синхронного вызова).
fn wait_ack(rx: mpsc::Receiver<Result<()>>) -> Result<()> {
    rx.recv().map_err(|_| anyhow!("inject-воркер недоступен (ack-канал закрыт)"))?
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
            Cmd::Full { text, method } => (text.chars().count(), method.as_str()),
            Cmd::Incr { next, .. } => (next.chars().count(), "incr"),
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
            let _ = clipboard_set_retry(&prev);
            PENDING.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Dry-режим: фиксируем задание в журнале, ничего не нажимая.
fn run_dry(cmd: &Cmd) -> Result<()> {
    let entry = match cmd {
        Cmd::Full { text, .. } => text.clone(),
        Cmd::Incr { prev, next } => format!("incr|{prev}|{next}"),
    };
    DRY_LOG.lock().push(entry);
    Ok(())
}

/// Боевое исполнение задания (только из воркера). Второй элемент — прежний
/// буфер обмена, который надо восстановить ПОСЛЕ ack (см. worker_loop).
fn run_real(enigo: &mut Option<Enigo>, cmd: &Cmd) -> (Result<()>, Option<String>) {
    match cmd {
        Cmd::Full { text, method } => match method.as_str() {
            "type" => (try_type(enigo, text), None),
            _ => {
                let mut restore = None;
                match paste_text(enigo, text, &mut restore) {
                    Ok(()) => (Ok(()), restore),
                    // если paste не сработал — пробуем печать
                    Err(e) => {
                        log::warn!("paste failed ({e}), fallback to type");
                        (try_type(enigo, text), restore)
                    }
                }
            }
        },
        Cmd::Incr { prev, next } => (
            enigo_of(enigo).and_then(|e| incremental_keys(e, prev, next)),
            None,
        ),
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

/// Вставка текста целиком. method: "clipboard" (дефолт, с fallback в печать) | "type".
/// Блокируется до фактического исполнения воркером — как и раньше, но теперь
/// нажатия идут из одного потока в порядке очереди.
pub fn inject(text: &str, method: &str) -> Result<()> {
    if text.trim().is_empty() {
        return Ok(());
    }
    wait_ack(enqueue(Cmd::Full { text: text.to_string(), method: method.to_string() }))
}

/// Инкрементальная вставка КЛАВИШАМИ (не через буфер обмена — paste не умеет
/// backspace). Сводит `prev` → `next`: считает длину общего ПОСИМВОЛЬНОГО
/// префикса, удаляет хвост `prev` (Backspace по разу на символ), затем печатает
/// хвост `next`. Блокируется до исполнения воркером.
pub fn inject_incremental(prev: &str, next: &str) -> Result<()> {
    if prev == next {
        return Ok(());
    }
    wait_ack(enqueue(Cmd::Incr { prev: prev.to_string(), next: next.to_string() }))
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

/// Записать текст в буфер с ретраями: clipboard на Windows — глобальный ресурс,
/// его может коротко держать другое приложение (ERROR_CLIPBOARD_BUSY и т.п.).
/// 3 попытки с паузой 30мс между ними.
fn clipboard_set_retry(text: &str) -> Result<()> {
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..3 {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(30));
        }
        match arboard::Clipboard::new() {
            Ok(mut cb) => match cb.set_text(text.to_string()) {
                Ok(()) => return Ok(()),
                Err(e) => last = Some(anyhow!("clipboard set: {e}")),
            },
            Err(e) => last = Some(anyhow!("clipboard open: {e}")),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("clipboard: неизвестная ошибка")))
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
fn paste_text(enigo_slot: &mut Option<Enigo>, text: &str, restore_out: &mut Option<String>) -> Result<()> {
    // Сохранить текущий буфер (best-effort: нет текста — нечего восстанавливать).
    let prev = arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok());

    // Положить наш текст (с ретраями ×3 по 30мс — см. clipboard_set_retry).
    clipboard_set_retry(text)?;
    // Дать ОС увидеть новый буфер до Ctrl+V (срезано с 40мс — set_text синхронен).
    thread::sleep(Duration::from_millis(25));

    // Послать Ctrl+V (Cmd+V на macOS) в активное окно.
    #[cfg(target_os = "macos")]
    let modkey = Key::Meta;
    #[cfg(not(target_os = "macos"))]
    let modkey = Key::Control;
    // V шлём РАСКЛАДКО-НЕЗАВИСИМО по virtual-key code, иначе на русской раскладке
    // enigo не может смаппить Unicode('v') в VK и тихо впрыскивает 'v' как
    // KEYEVENTF_UNICODE-символ, который Windows не считает частью Ctrl-аккорда —
    // и вставка не срабатывает. Key::Other(VK) на Windows = сырой virtual-key.
    #[cfg(windows)]
    let vkey = Key::Other(0x56); // VK_V
    #[cfg(not(windows))]
    let vkey = Key::Unicode('v');

    // --- до этой черты ошибки безопасны (V ещё не доставлена) ---
    let e = enigo_of(enigo_slot)?;
    e.key(modkey, Direction::Press).map_err(|e| anyhow!("mod down: {e}"))?;
    if let Err(err) = e.key(vkey, Direction::Click) {
        // V не доставлена — откатываем модификатор и сообщаем об ошибке, fallback в печать безопасен.
        let _ = e.key(modkey, Direction::Release);
        return Err(anyhow!("v: {err}"));
    }
    // --- V ОТПРАВЛЕНА: дальше только best-effort, возвращаем строго Ok ---
    // Отпускание модификатора best-effort: ошибка здесь НЕ должна вызвать дубль.
    let _ = e.key(modkey, Direction::Release);

    // Короткий settle: дать окну принять аккорд (текст появляется в поле уже
    // здесь). Прежний буфер возвращаем НЕ тут, а после ack в worker_loop —
    // суммарная пауза V→restore остаётся 15+115=130мс (компромисс 140↔90:
    // тяжёлые Electron/Chromium-приёмники читают буфер асинхронно и при 90мс
    // изредка вставляли СТАРЫЙ буфер), но вызывающий больше эти 130мс не ждёт.
    thread::sleep(Duration::from_millis(15));
    *restore_out = prev;
    Ok(())
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
                        });
                        expected.lock().push(text);
                        rx
                    };
                    rx.recv().expect("ack-канал жив").expect("dry-job без ошибок");
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
}
