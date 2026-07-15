//! Движок диктовки. Владеет потоком захвата (cpal Stream — !Send) и оркестрирует
//! полный цикл: запись → ресемпл → VAD → ASR → постобработка → инжект → статистика/события.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rusqlite::Connection;
use tauri::{AppHandle, Emitter};

use crate::app_context::TargetFingerprint;
use crate::asr::{self, AsrParams};
use crate::audio::{self, Capture};
use crate::settings::Settings;
use crate::system_audio::AutoMuteGuard;
use crate::{db, inject, paths, postprocess};
use std::collections::{HashMap, HashSet, VecDeque};

/// Звуки старт/стоп (Windows: MessageBeep, без зависимостей; неблокирующий).
#[cfg(windows)]
mod sound {
    use std::time::Duration;

    #[link(name = "kernel32")]
    extern "system" {
        fn Beep(dwFreq: u32, dwDuration: u32) -> i32;
    }
    #[link(name = "user32")]
    extern "system" {
        fn MessageBeep(u_type: u32) -> i32;
    }
    pub fn play(start: bool) {
        // 0x40 = MB_ICONASTERISK (старт), 0x00 = MB_OK (стоп)
        unsafe {
            MessageBeep(if start { 0x40 } else { 0x00 });
        }
    }
    pub fn latch() {
        std::thread::spawn(|| unsafe {
            let _ = Beep(740, 48);
            std::thread::sleep(Duration::from_millis(28));
            let _ = Beep(988, 64);
        });
    }
    pub fn fail() {
        // 0x10 = MB_ICONHAND (звук ошибки) — «не расслышал»
        unsafe {
            MessageBeep(0x10);
        }
    }
}
#[cfg(not(windows))]
mod sound {
    pub fn play(_start: bool) {}
    pub fn latch() {}
    pub fn fail() {}
}

/// Команды движку (из хоткея, трея, UI).
pub enum EngineCmd {
    Start,
    /// Второй down быстрого double-tap: показать latch-подтверждение до
    /// синхронного открытия микрофона/снятия контекста, затем начать запись.
    StartLatched,
    Stop,
    /// Первый короткий tap возможного double-tap. Запись завершается и
    /// отбрасывается без запуска финального ASR.
    StopTap,
    Toggle,
    Cancel,
    ImproveSelection,
    /// Privacy-safe signal from the global keyboard hook: the user changed
    /// text shortly after a VoxFlow insertion. The actual key/character is
    /// deliberately not carried or logged; the engine reads the focused field
    /// through Accessibility/UIA after a short debounce.
    ManualEdit,
    /// Фоновый прогрев движков под ТЕКУЩИЕ настройки (шлётся после смены
    /// языка из трея/UI): без него первый Start после переключения на en/auto
    /// синхронно грузит ~650 МБ Parakeet и подвешивает поток движка.
    Warmup,
    Shutdown,
}

const PARAGRAPH_GAP_SAMPLES: usize = 8 * 16000;
const DICTATION_CONTEXT_RECENT_LIMIT: usize = 6;
const DICTATION_CONTEXT_RECENT_CHARS: usize = 1200;
const DICTATION_CONTEXT_SUMMARY_CHARS: usize = 700;
const DICTATION_CONTEXT_ITEM_CHARS: usize = 360;
const ASR_PROMPT_MAX_CHARS: usize = 1100;
const ASR_PROMPT_PREVIOUS_CHARS: usize = 280;
const ASR_PROMPT_TERM_LIMIT: usize = 36;
const ASR_PROMPT_SNIPPET_LIMIT: usize = 12;
const ASR_PROMPT_CORRECTION_LIMIT: usize = 16;
const TYPED_CORRECTION_WINDOW: Duration = Duration::from_secs(120);
const TYPED_CORRECTION_DEBOUNCE: Duration = Duration::from_millis(700);
const TYPED_CORRECTION_CONFIRM: Duration = Duration::from_millis(300);
const TYPED_CORRECTION_FIELD_LIMIT: usize = 1600;
const BUILTIN_ASR_TERMS: &[&str] = &[
    "VoxFlow",
    "Wispr Flow",
    "Aqua Voice",
    "Tauri",
    "Rust",
    "whisper.cpp",
    "Codex",
    "OpenAI",
    "Deepgram",
    "Gemini",
    "GigaAM",
    "Parakeet",
];

#[derive(Clone)]
pub struct EngineHandle {
    auto_mute: Arc<Mutex<Option<AutoMuteGuard>>>,
    cancel_active: Arc<AtomicBool>,
    correction_capture_active: Arc<AtomicBool>,
}

impl EngineHandle {
    pub fn restore_auto_mute(&self) {
        restore_auto_mute_arc(&self.auto_mute);
    }

    /// Общий с hotkey признак реально отменяемой работы: активная запись,
    /// финальная обработка или улучшение выделения.
    pub fn cancel_active_flag(&self) -> Arc<AtomicBool> {
        self.cancel_active.clone()
    }

    /// Shared with the keyboard hook. It is true only during the short,
    /// post-insert learning window, so ordinary global typing is ignored.
    pub fn correction_capture_flag(&self) -> Arc<AtomicBool> {
        self.correction_capture_active.clone()
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        self.restore_auto_mute();
    }
}

/// Состояние ОДНОЙ диктовки для живого стриминга частичных результатов.
/// Создаётся заново в `start_capture_into` на каждый Start, кладётся в
/// `EngineCtx.partial`, чтобы `stop_and_process` мог завершить петлю и продолжить
/// инкрементальную вставку из `injected`/`committed`.
struct PartialState {
    /// Флаг остановки петли частичных результатов.
    stop: Arc<AtomicBool>,
    /// Хэндл потока петли: завершившийся join забираем, занятый безопасно детачим,
    /// чтобы Stop никогда не блокировал следующий Start.
    join: Option<JoinHandle<()>>,
    /// Что физически НАПЕЧАТАНО в поле за эту диктовку (always: prev для inject_incremental).
    injected: Arc<Mutex<String>>,
    /// auto: уже зафиксированный (стабильный) префикс, напечатанный в поле.
    committed: Arc<Mutex<String>>,
    /// Сработал ли запрет живой вставки (сменилось окно/поле) — остаётся true до конца диктовки.
    abort: Arc<AtomicBool>,
    /// Активное окно на старте — отпечаток целевого поля.
    start_fp: TargetFingerprint,
    /// Режим вставки на момент старта: "never" | "auto" | "always".
    stream_mode: String,
    /// Чем занята петля: whisper-server требует аккуратного join/kill перед
    /// финалом, локальные/облачные черновики можно детачить быстрее.
    kind: PartialKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PartialKind {
    WhisperServer,
    Local,
    Cloud,
}

fn finish_partial_without_blocking(join: JoinHandle<()>, kind: PartialKind) {
    if join.is_finished() {
        let _ = join.join();
        return;
    }
    dbg_log(match kind {
        PartialKind::WhisperServer => {
            "stop: whisper partial ещё занят — детачим без блокировки следующего Start"
        }
        PartialKind::Local | PartialKind::Cloud => {
            "stop: preview ещё занят — детачим без блокировки следующего Start"
        }
    });
    // Dropping JoinHandle detaches; the stop flag makes the worker exit at its
    // next cancellation point without holding the engine command queue.
}

#[cfg(any(test, target_os = "macos"))]
fn insertion_permission_blocks_capture(is_macos: bool, post_event_allowed: bool) -> bool {
    is_macos && !post_event_allowed
}

const NORECOG_FEEDBACK_MIN_CAPTURE_MS: u64 = 500;

fn should_emit_norecog(capture_ms: u64) -> bool {
    // Первый tap double-tap жеста обычно даёт 150–400 мс буфера (включая
    // macOS target lookup). Это управляющий жест, а не неудачная диктовка.
    capture_ms >= NORECOG_FEEDBACK_MIN_CAPTURE_MS
}

#[derive(Default)]
struct DictationMemory {
    target_fp: Option<DictationMemoryTarget>,
    summary: String,
    recent: VecDeque<DictationMemoryItem>,
}

#[derive(Clone, PartialEq, Eq)]
struct DictationMemoryTarget {
    exe: String,
    title: String,
    window_id: String,
    field_id: String,
}

#[derive(Clone)]
struct DictationMemoryItem {
    text: String,
    hard_boundary: bool,
    truncated_start: bool,
}

struct LastInject {
    text: String,
    at: Instant,
    target_fp: TargetFingerprint,
    field_id: String,
    field_role: String,
    field_subrole: String,
    field_before: String,
    manual_edit_at: Option<Instant>,
    pending_observation: Option<TypedCorrectionObservation>,
}

struct TypedCorrectionObservation {
    field_text: String,
    edit_epoch: Instant,
    observed_at: Instant,
}

impl LastInject {
    fn mark_manual_edit(&mut self, now: Instant) {
        self.manual_edit_at = Some(now);
        // Any new mutating key invalidates snapshots captured for the previous
        // typing epoch. The hook intentionally retains no key or character.
        self.pending_observation = None;
    }
}

#[derive(Clone)]
struct EngineCtx {
    app: AppHandle,
    db: Arc<Mutex<Connection>>,
    settings: Arc<Mutex<Settings>>,
    recording: Arc<AtomicBool>,
    /// Постоянный whisper-server (если используется движок whisper_server).
    server: Arc<Mutex<Option<asr::Server>>>,
    /// A failed bundled CUDA runtime is skipped for the rest of the process.
    /// This prevents every dictation from paying the same driver/JIT timeout;
    /// the independently packaged CPU runtime remains available.
    whisper_accelerated_disabled: Arc<AtomicBool>,
    /// Последний вставленный текст — для авто-захвата исправлений из буфера обмена.
    last_inject: Arc<Mutex<Option<LastInject>>>,
    /// Keyboard hook is active only after our own successful insertion. The
    /// signal contains no key data and is closed on timeout/window change.
    correction_capture_active: Arc<AtomicBool>,
    /// Сериализация ВСЕХ обращений к /inference whisper-server (тики partial + финал).
    /// Один на движок, переиспользуется каждую диктовку.
    asr_lock: Arc<Mutex<()>>,
    /// Сериализация ВСЕЙ эмиссии клавиш (живые вставки тиков + финальная реконсиляция +
    /// обычная вставка). Не даёт нажатиям ДВУХ диктовок (быстрый рестарт латчем/двойным
    /// тапом, когда detached-поток финала прошлой диктовки ещё печатает) чередоваться и
    /// портить текст в целевом поле.
    inject_lock: Arc<Mutex<()>>,
    /// Состояние живого стриминга текущей диктовки (None, если петля не запущена).
    partial: Arc<Mutex<Option<PartialState>>>,
    /// Physical live text transferred to a detached final. The slot is an
    /// ownership token: a final commit/reject, a new Start, or Esc may claim it
    /// exactly once while holding the keyboard + generation locks.
    pending_live_draft: Arc<Mutex<Option<PendingLiveDraft>>>,
    /// Целевое окно текущей записи, снятое ДО показа overlay/status.
    active_target: Arc<Mutex<Option<TargetFingerprint>>>,
    /// Последнее внешнее окно, куда можно безопасно возвращать фокус для вставки.
    ///
    /// macOS может на короткое время сделать frontmost наше окно/оверлей или
    /// системное предупреждение. Без этой памяти старт диктовки ошибочно целился
    /// в VoxFlow и финал не вставлялся в пользовательское поле.
    last_external_target: Arc<Mutex<Option<TargetFingerprint>>>,
    /// «Поколение» диктовки. Инкрементируется на каждый старт захвата. Detached-поток
    /// финала запоминает своё поколение и перед вставкой сверяет его с текущим: если
    /// уже стартовала НОВАЯ диктовка (gen вырос) — «осиротевший» поток НЕ вставляет
    /// (защита от многократной вставки при быстрой диктовке подряд, C4). Также служит
    /// суффиксом уникального имени временного WAV (изоляция от гонки на общем файле).
    gen: Arc<AtomicU64>,
    /// Поколение, для которого финальная вставка УЖЕ выполнена. Идемпотентность
    /// вставки: одно поколение вставляется РОВНО один раз (пояс поверх gen-guard) —
    /// даже если два detached-потока финала как-то совпали по gen, второй НЕ дублирует
    /// текст в активном поле. Это вторая линия защиты от «дубля вставки».
    last_injected_gen: Arc<AtomicU64>,
    /// Резидентный GigaAM-v3 (русский ASR, ONNX/CPU): грузится на warmup и живёт
    /// всю сессию — холодного старта на диктовке нет. Mutex сериализует
    /// партиал-тики и финал (Session::run требует &mut).
    gigaam: Arc<Mutex<Option<crate::gigaam::GigaAm>>>,
    /// Резидентный Parakeet TDT v3 (en + автодетект языка): тот же жизненный цикл,
    /// что у gigaam — ленивый ensure + warmup, ЕСЛИ модель установлена и
    /// language ∈ {en, auto}. Автоматически НЕ скачивается.
    parakeet: Arc<Mutex<Option<crate::parakeet::Parakeet>>>,
    /// Резидентный Silero VAD для ПАРТИАЛ-петли (несёт стриминговый state
    /// поверх тиков — его нельзя сбрасывать чужими вызовами).
    vad: Arc<Mutex<Option<crate::vad::SileroVad>>>,
    /// Отдельный VAD для ФИНАЛОВ (has_speech-гейт, сегментация длинного аудио).
    /// Финал — detached-поток и при быстром рестарте перекрывается с петлёй
    /// СЛЕДУЮЩЕЙ диктовки; общий инстанс ломал бы её стриминговый state.
    vad_final: Arc<Mutex<Option<crate::vad::SileroVad>>>,
    /// Не даёт запускать несколько улучшений выделенного текста одновременно.
    improve_busy: Arc<AtomicBool>,
    /// Короткая память последних финальных вставок для того же окна: помогает
    /// LLM-рерайту продолжать предложение и ставить пунктуацию по контексту.
    dictation_memory: Arc<Mutex<DictationMemory>>,
    /// Guard системного mute на время активной диктовки.
    auto_mute: Arc<Mutex<Option<AutoMuteGuard>>>,
    /// `Esc` имеет смысл только пока действительно идёт запись/финал/улучшение.
    /// Отдельный флаг нужен, потому что `recording=false` уже во время финального ASR.
    cancel_active: Arc<AtomicBool>,
    /// Сериализует смену поколения и `cancel_active` между engine-loop и detached
    /// финалами, чтобы завершение старого финала не очистило флаг новой записи.
    cancel_activity_lock: Arc<Mutex<()>>,
}

/// Поднять рабочий поток движка.
pub fn spawn(
    app: AppHandle,
    rx: Receiver<EngineCmd>,
    db: Arc<Mutex<Connection>>,
    settings: Arc<Mutex<Settings>>,
    recording: Arc<AtomicBool>,
) -> EngineHandle {
    let auto_mute = Arc::new(Mutex::new(None));
    let cancel_active = Arc::new(AtomicBool::new(false));
    let correction_capture_active = Arc::new(AtomicBool::new(false));
    let ctx = EngineCtx {
        app,
        db,
        settings,
        recording,
        server: Arc::new(Mutex::new(None)),
        whisper_accelerated_disabled: Arc::new(AtomicBool::new(false)),
        last_inject: Arc::new(Mutex::new(None)),
        correction_capture_active: correction_capture_active.clone(),
        asr_lock: Arc::new(Mutex::new(())),
        inject_lock: Arc::new(Mutex::new(())),
        partial: Arc::new(Mutex::new(None)),
        pending_live_draft: Arc::new(Mutex::new(None)),
        active_target: Arc::new(Mutex::new(None)),
        last_external_target: Arc::new(Mutex::new(None)),
        gen: Arc::new(AtomicU64::new(0)),
        last_injected_gen: Arc::new(AtomicU64::new(0)),
        gigaam: Arc::new(Mutex::new(None)),
        parakeet: Arc::new(Mutex::new(None)),
        vad: Arc::new(Mutex::new(None)),
        vad_final: Arc::new(Mutex::new(None)),
        improve_busy: Arc::new(AtomicBool::new(false)),
        dictation_memory: Arc::new(Mutex::new(DictationMemory::default())),
        auto_mute: auto_mute.clone(),
        cancel_active: cancel_active.clone(),
        cancel_activity_lock: Arc::new(Mutex::new(())),
    };
    // Прогрев whisper-server в фоне (CUDA JIT один раз → первая диктовка тоже быстрая).
    let warm = ctx.clone();
    std::thread::spawn(move || warmup(warm));
    // Память внешнего окна: помогает диктовать из фона, даже если собственная
    // панель или macOS privacy-alert коротко перехватили frontmost.
    let target_watch = ctx.clone();
    std::thread::spawn(move || external_target_watcher(target_watch));
    std::thread::Builder::new()
        .name("voxflow-engine".into())
        .spawn(move || engine_loop(rx, ctx))
        .expect("spawn engine thread");
    EngineHandle {
        auto_mute,
        cancel_active,
        correction_capture_active,
    }
}

/// Простой файловый лог для диагностики (data_dir/debug.log).
pub fn dbg_log(msg: &str) {
    use std::io::Write;
    let p = paths::data_dir().join("debug.log");
    if let Ok(mut f) = paths::open_private_append(&p) {
        let now = chrono::Local::now().format("%H:%M:%S%.3f");
        let _ = writeln!(f, "[{now}] {msg}");
    }
}

fn remember_external_target(ctx: &EngineCtx, fp: &TargetFingerprint) {
    if fp.is_usable_dictation_target() {
        *ctx.last_external_target.lock() = Some(fp.clone());
    }
}

fn detect_context_with_retry(
    mut detect: impl FnMut() -> crate::app_context::AppContext,
    attempts: usize,
    retry_delay: Duration,
) -> crate::app_context::AppContext {
    let attempts = attempts.max(1);
    let mut current = detect();
    for _ in 1..attempts {
        if !current.is_unknown() {
            break;
        }
        if !retry_delay.is_zero() {
            std::thread::sleep(retry_delay);
        }
        current = detect();
    }
    current
}

fn detect_current_context() -> crate::app_context::AppContext {
    let attempts = if cfg!(target_os = "macos") { 3 } else { 1 };
    detect_context_with_retry(
        crate::app_context::detect,
        attempts,
        Duration::from_millis(15),
    )
}

fn resolve_start_target_from_context(
    ctx: &EngineCtx,
    detected_context: &crate::app_context::AppContext,
) -> TargetFingerprint {
    let detected = detected_context.target_fingerprint();
    if detected.is_usable_dictation_target() {
        remember_external_target(ctx, &detected);
        return detected;
    }
    if let Some(prev) = ctx.last_external_target.lock().clone() {
        dbg_log(&format!(
            "start: frontmost {} не цель диктовки — используем последнее внешнее окно {}",
            detected.describe(),
            prev.describe()
        ));
        return prev;
    }
    dbg_log(&format!(
        "start: внешняя цель неизвестна, frontmost {}",
        detected.describe()
    ));
    detected
}

fn external_target_watcher(ctx: EngineCtx) {
    loop {
        if external_target_watcher_should_detect(
            ctx.recording.load(Ordering::SeqCst),
            ctx.cancel_active.load(Ordering::SeqCst),
        ) {
            let current = detect_current_context();
            maybe_learn_typed_correction(&ctx, &current);
            let fp = current.target_fingerprint();
            remember_external_target(&ctx, &fp);
        }
        std::thread::sleep(external_target_watcher_interval());
    }
}

fn external_target_watcher_should_detect(recording: bool, action_active: bool) -> bool {
    !recording && !action_active
}

fn external_target_watcher_interval() -> Duration {
    // macOS detection launches System Events through osascript. Polling it every
    // 350 ms caused process contention and hundreds of milliseconds of avoidable
    // release-to-insert latency. Windows uses direct WinAPI and stays responsive
    // at the original cadence.
    if cfg!(target_os = "macos") {
        Duration::from_millis(1000)
    } else {
        Duration::from_millis(350)
    }
}

#[cfg(test)]
mod external_target_watcher_tests {
    use super::*;

    fn context(exe: &str, window_id: &str) -> crate::app_context::AppContext {
        crate::app_context::AppContext {
            exe: exe.into(),
            title: exe.into(),
            window_id: window_id.into(),
            category: "neutral".into(),
            field_role: String::new(),
            field_subrole: String::new(),
            field_id: String::new(),
            field_text: String::new(),
            selected_text: String::new(),
        }
    }

    #[test]
    fn watcher_only_detects_while_fully_idle() {
        assert!(external_target_watcher_should_detect(false, false));
        assert!(!external_target_watcher_should_detect(true, false));
        assert!(!external_target_watcher_should_detect(false, true));
        assert!(!external_target_watcher_should_detect(true, true));
    }

    #[test]
    fn watcher_uses_platform_appropriate_cadence() {
        let expected = if cfg!(target_os = "macos") { 1000 } else { 350 };
        assert_eq!(
            external_target_watcher_interval(),
            Duration::from_millis(expected)
        );
    }

    #[test]
    fn transient_empty_detection_is_retried() {
        let mut sequence = vec![context("", ""), context("telegram", "pid=42")].into_iter();
        let detected = detect_context_with_retry(
            || sequence.next().expect("enough samples"),
            3,
            Duration::ZERO,
        );
        assert_eq!(detected.exe, "telegram");
        assert!(sequence.next().is_none());
    }

    #[test]
    fn confirmed_target_is_not_retried() {
        let mut calls = 0;
        let detected = detect_context_with_retry(
            || {
                calls += 1;
                context("chrome", "pid=99")
            },
            3,
            Duration::ZERO,
        );
        assert_eq!(detected.exe, "chrome");
        assert_eq!(calls, 1);
    }

    #[test]
    fn repeated_empty_detection_stays_unknown_after_bound() {
        let mut calls = 0;
        let detected = detect_context_with_retry(
            || {
                calls += 1;
                context("", "")
            },
            3,
            Duration::ZERO,
        );
        assert!(detected.is_unknown());
        assert_eq!(calls, 3);
    }
}

fn current_or_restored_target(
    ctx: &EngineCtx,
    target_fp: &mut TargetFingerprint,
    stage: &str,
) -> Option<crate::app_context::AppContext> {
    let mut cur = detect_current_context();
    let mut cur_fp = cur.target_fingerprint();
    if target_fp.matches(&cur) {
        remember_external_target(ctx, target_fp);
        return Some(cur);
    }

    if target_fp.is_own_app() && cur.is_usable_dictation_target() {
        dbg_log(&format!(
            "финал: target был VoxFlow ({stage}) — переносим цель на текущее внешнее окно {}",
            cur_fp.describe()
        ));
        *target_fp = cur_fp.clone();
        remember_external_target(ctx, target_fp);
        return Some(cur);
    }

    if (cur.is_own_app() || cur.is_transient_system_ui() || cur.is_unknown())
        && target_fp.is_usable_dictation_target()
        && activate_target_for_insert(target_fp)
    {
        let deadline = Instant::now() + Duration::from_millis(650);
        loop {
            std::thread::sleep(Duration::from_millis(25));
            cur = detect_current_context();
            cur_fp = cur.target_fingerprint();
            if target_fp.matches(&cur) {
                dbg_log(&format!(
                    "финал: восстановили фокус целевого окна ({stage}) {}",
                    target_fp.describe()
                ));
                remember_external_target(ctx, target_fp);
                return Some(cur);
            }
            if Instant::now() >= deadline
                || (!cur.is_unknown() && !cur.is_own_app() && !cur.is_transient_system_ui())
            {
                break;
            }
        }
    }

    if target_fp.is_own_app() {
        dbg_log(&format!(
            "финал: нет внешней цели для вставки ({stage}); current={}",
            cur_fp.describe()
        ));
        emit_error(
            &ctx.app,
            "Поставьте курсор в поле текста и запустите диктовку из фона",
        );
        return None;
    }

    dbg_log(&format!(
        "финал: окно изменилось ({stage}) — вставка отменена; target={} current={}",
        target_fp.describe(),
        cur_fp.describe()
    ));
    None
}

#[cfg(target_os = "macos")]
fn activate_target_for_insert(target_fp: &TargetFingerprint) -> bool {
    if let Some(pid) = target_fp.macos_pid() {
        if let Some(application) =
            objc2_app_kit::NSRunningApplication::runningApplicationWithProcessIdentifier(pid as i32)
        {
            if application
                .activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::empty())
            {
                return true;
            }
        }
    }

    let Some(bundle) = target_fp.macos_bundle_id() else {
        return false;
    };
    let bundle = bundle.replace('"', "");
    let script = format!(
        r#"try
  tell application id "{bundle}" to activate
  return "ok"
on error
  return "err"
end try"#
    );
    run_osascript_ok(&script)
}

#[cfg(target_os = "macos")]
fn run_osascript_ok(script: &str) -> bool {
    let mut child = match std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = Instant::now() + Duration::from_millis(900);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                dbg_log("macOS target activation timed out");
                return false;
            }
        }
    }
    let out = child.wait_with_output();
    match out {
        Ok(out) if out.status.success() => {
            let body = String::from_utf8_lossy(&out.stdout);
            body.contains("ok")
        }
        _ => false,
    }
}

#[cfg(windows)]
fn activate_target_for_insert(target_fp: &TargetFingerprint) -> bool {
    #[link(name = "user32")]
    extern "system" {
        fn IsWindow(hwnd: isize) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
    }

    let Some(hwnd) = target_fp.windows_hwnd() else {
        return false;
    };
    // This path is used when a VoxFlow window owns foreground activation (for
    // example after clicking Flow Bar), so Windows permits the foreground owner
    // to hand focus back to the previously captured target HWND.
    unsafe { IsWindow(hwnd) != 0 && SetForegroundWindow(hwnd) != 0 }
}

#[cfg(not(any(target_os = "macos", windows)))]
fn activate_target_for_insert(_target_fp: &TargetFingerprint) -> bool {
    false
}

/// Абстракция локального РЕЗИДЕНТНОГО STT (ort, живёт в памяти всю сессию):
/// GigaAM и Parakeet. Партиал-петля и сегментный финал становятся generic.
/// Whisper (sidecar-процесс, вход WAV) и облако на трейт НЕ натягиваем —
/// у них другой жизненный цикл и вход (см. PLAN §2).
pub(crate) trait LocalStt {
    fn transcribe(&mut self, samples_16k: &[f32]) -> anyhow::Result<String>;
}
impl LocalStt for crate::gigaam::GigaAm {
    fn transcribe(&mut self, samples_16k: &[f32]) -> anyhow::Result<String> {
        crate::gigaam::GigaAm::transcribe(self, samples_16k)
    }
}
impl LocalStt for crate::parakeet::Parakeet {
    fn transcribe(&mut self, samples_16k: &[f32]) -> anyhow::Result<String> {
        crate::parakeet::Parakeet::transcribe(self, samples_16k)
    }
}

/// Preview startup must never wait for a resident model to load. `Start` runs
/// on the single engine command thread; blocking here also delays a queued
/// `Stop`, which is especially visible during rapid key presses. Background
/// warmup/final ASR may own the mutex, so use `try_lock` and skip this optional
/// preview for the current utterance when the model is not already resident.
fn resident_model_ready<T>(engine: &Arc<Mutex<Option<T>>>) -> bool {
    engine
        .try_lock()
        .map(|guard| guard.is_some())
        .unwrap_or(false)
}

/// Маршрут локального распознавания по настройкам (роутер языков, PLAN §2).
/// Считается заново на каждый старт/финал — установка модели Parakeet
/// подхватывается без перезапуска.
#[derive(Clone, Copy, PartialEq, Debug)]
enum LocalRoute {
    /// ru + движок gigaam — как раньше.
    GigaAm,
    /// Явный en при установленном Parakeet.
    Parakeet,
    /// Всё остальное (auto/прочие языки/whisper-движки).
    Whisper,
}

fn is_auto_language_alias(language: &str) -> bool {
    matches!(
        language.trim().to_ascii_lowercase().as_str(),
        "auto" | "all" | "any" | "multi" | "multilingual" | "*"
    )
}

fn local_route(s: &Settings) -> LocalRoute {
    let parakeet_ready = crate::parakeet::dir_ready(&paths::parakeet_dir());
    local_route_with_parakeet(s, parakeet_ready)
}

fn local_route_with_parakeet(s: &Settings, parakeet_ready: bool) -> LocalRoute {
    if s.engine != "gigaam" {
        return LocalRoute::Whisper;
    }
    match s.language.trim().to_ascii_lowercase().as_str() {
        "ru" | "russian" => LocalRoute::GigaAm,
        "en" | "english" if parakeet_ready => LocalRoute::Parakeet,
        _ => LocalRoute::Whisper,
    }
}

/// Бейдж языка по скрипту текста (контракт overlay): кириллица → "ru",
/// латиница → "en", не разобрать (пусто/цифры) → None (бейдж скрыт).
fn detect_lang_label(text: &str) -> Option<&'static str> {
    if text.trim().is_empty() {
        return None;
    }
    if crate::parakeet::is_mostly_cyrillic(text) {
        return Some("ru");
    }
    if text.chars().any(|c| c.is_ascii_alphabetic()) {
        return Some("en");
    }
    None
}

fn word_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| w.chars().any(char::is_alphabetic))
        .count()
}

fn has_cyrillic(text: &str) -> bool {
    text.chars()
        .any(|c| ('а'..='я').contains(&c.to_ascii_lowercase()) || c == 'ё' || c == 'Ё')
}

fn prefer_gigaam_for_auto(whisper_text: &str, gigaam_text: &str) -> bool {
    let g = gigaam_text.trim();
    if g.is_empty() || !crate::parakeet::is_mostly_cyrillic(g) {
        return false;
    }
    let w = whisper_text.trim();
    if w.is_empty() {
        return true;
    }
    let gw = word_count(g);
    let ww = word_count(w);
    if crate::parakeet::is_mostly_cyrillic(w) {
        // A coherent multilingual Whisper result must not be replaced merely
        // because GigaAM produced a similarly long Russian string. Use GigaAM
        // only when Whisper is clearly truncated.
        return ww <= 2 && gw >= 4 && gw >= ww.saturating_mul(2);
    }
    // Типичный сбой whisper auto на русской речи: короткая латинская фраза
    // вроде "After" / "Państwo, unze" вместо полноценной русской диктовки.
    !has_cyrillic(w) && gw >= 3 && (ww <= 2 || gw >= ww.saturating_mul(2))
}

fn should_probe_gigaam_for_auto(whisper_text: &str) -> bool {
    let whisper = whisper_text.trim();
    if whisper.is_empty() {
        return true;
    }
    if crate::parakeet::is_mostly_cyrillic(whisper) {
        // A coherent Russian Whisper phrase can never be replaced by
        // `prefer_gigaam_for_auto`; do not pay for a guaranteed-unused second
        // full ASR pass. Only a very short/truncated result remains eligible.
        return word_count(whisper) <= 2;
    }
    // Mixed-script text already contains useful Cyrillic context and also can
    // never satisfy the fallback selector. Pure Latin gibberish remains the
    // recoverable failure mode.
    !has_cyrillic(whisper)
}

const DEFAULT_MULTILINGUAL_PROMPT: &str = "Multilingual speech recognition. Preserve Russian, English and other language switches. Use punctuation, but do not add a final period merely because recording stopped when the sentence is grammatically unfinished. Keep technical terms such as VoxFlow, Tauri, whisper.cpp and Codex.";

fn whisper_base_prompt(language: &str) -> Option<&'static str> {
    match language.trim().to_ascii_lowercase().as_str() {
        "ru" | "russian" => Some(postprocess::DEFAULT_RU_PROMPT),
        _ => None,
    }
    .or_else(|| is_auto_language_alias(language).then_some(DEFAULT_MULTILINGUAL_PROMPT))
}

fn whisper_language_arg(language: &str) -> String {
    if is_auto_language_alias(language) {
        "auto".into()
    } else {
        language.trim().to_string()
    }
}

/// Заранее поднять и прогреть резидентные модели (GigaAM/Parakeet/VAD или
/// whisper-server), чтобы первая диктовка не ждала загрузку/JIT.
fn warmup(ctx: EngineCtx) {
    // This already runs off the engine command thread. Delaying it by 1.2 s
    // widened the interval in which the first dictation collided with a cold
    // model/JIT warmup; start immediately so the resident server is useful by
    // the time the user reaches the hotkey.
    let s = ctx.settings.lock().clone();
    dbg_log(&format!("warmup: engine={}, model={}", s.engine, s.model));
    // VAD грузим всегда (2 МБ, мгновенно) — гейт тишины нужен во всех режимах.
    // Два инстанса: петля партиалов (стриминговый state) и финалы — раздельно.
    {
        let t = Instant::now();
        let p = paths::vad_model_path(Some(&ctx.app));
        match (
            crate::vad::SileroVad::load(&p),
            crate::vad::SileroVad::load(&p),
        ) {
            (Ok(v1), Ok(v2)) => {
                *ctx.vad.lock() = Some(v1);
                *ctx.vad_final.lock() = Some(v2);
                dbg_log(&format!("warmup: vad×2 за {} мс", t.elapsed().as_millis()));
            }
            (r1, r2) => dbg_log(&format!(
                "warmup: vad ОШИБКА: {:?}/{:?}",
                r1.err().map(|e| e.to_string()),
                r2.err().map(|e| e.to_string())
            )),
        }
    }
    // Прогрев резидентного движка по маршруту (роутер языков, PLAN §2).
    let warm_gigaam = |ctx: &EngineCtx| match ensure_gigaam(ctx, &s) {
        Ok(()) => {
            if let Some(g) = ctx.gigaam.lock().as_mut() {
                let t = Instant::now();
                let _ = g.transcribe(&vec![0.0f32; 8000]);
                dbg_log(&format!(
                    "warmup: gigaam прогрет за {} мс",
                    t.elapsed().as_millis()
                ));
            }
        }
        Err(e) => dbg_log(&format!(
            "warmup: gigaam ОШИБКА: {e:#} (модель скачается при первом запуске)"
        )),
    };
    let warm_parakeet = |ctx: &EngineCtx| match ensure_parakeet(ctx, &s) {
        Ok(()) => {
            if let Some(p) = ctx.parakeet.lock().as_mut() {
                let t = Instant::now();
                let _ = p.transcribe(&vec![0.0f32; 8000]);
                dbg_log(&format!(
                    "warmup: parakeet прогрет за {} мс",
                    t.elapsed().as_millis()
                ));
            }
        }
        Err(e) => dbg_log(&format!("warmup: parakeet ОШИБКА: {e:#}")),
    };
    match local_route(&s) {
        LocalRoute::GigaAm => {
            // Основной путь: резидентный GigaAM. whisper-server не поднимаем —
            // он нужен только для фолбэка и стартует лениво.
            warm_gigaam(&ctx);
            return;
        }
        LocalRoute::Parakeet => {
            warm_parakeet(&ctx);
            return;
        }
        LocalRoute::Whisper => {}
    }
    if s.language == "auto"
        && s.engine == "gigaam"
        && crate::gigaam::dir_ready(&paths::gigaam_dir())
    {
        warm_gigaam(&ctx);
    }
    if s.engine == "whisper_cli" {
        dbg_log("warmup: cli — пропуск");
        return;
    }
    let model = match resolve_model(&s) {
        Ok(m) => m,
        Err(e) => {
            dbg_log(&format!("warmup: resolve_model ОШИБКА: {e}"));
            return;
        }
    };
    dbg_log(&format!("warmup: model={model:?}"));
    let wav = paths::tmp_dir().join("warmup.wav");
    if let Err(e) = audio::write_wav_16k_mono(&wav, &vec![0.0f32; 8000]) {
        dbg_log(&format!("warmup: wav ОШИБКА: {e}"));
        return;
    }
    let language = whisper_language_arg(&s.language);
    for (index, runtime) in whisper_runtime_dirs(&ctx).iter().enumerate() {
        if ctx.recording.load(Ordering::Acquire) {
            dbg_log("warmup: отменён начавшейся диктовкой");
            break;
        }
        dbg_log(&format!("warmup: whisper_dir={runtime:?}"));
        match ensure_server_cancellable(
            &ctx,
            runtime,
            &model,
            s.effective_threads(),
            ctx.recording.as_ref(),
        ) {
            Ok(port) => {
                if ctx.recording.load(Ordering::Acquire) {
                    dbg_log("warmup: сервер готов, но началась диктовка — прогрев пропущен");
                    break;
                }
                let Some(_asr_guard) = ctx.asr_lock.try_lock() else {
                    dbg_log("warmup: ASR уже занят — прогрев пропущен");
                    break;
                };
                if ctx.recording.load(Ordering::Acquire) {
                    dbg_log("warmup: диктовка началась перед inference — прогрев пропущен");
                    break;
                }
                dbg_log(&format!("warmup: сервер на {port}, прогрев..."));
                let result = asr::transcribe_server(port, &wav, &language, None);
                if result.is_ok() {
                    if index > 0 {
                        dbg_log("warmup: CUDA runtime failed, CPU fallback warmed");
                    }
                    dbg_log("warmup: прогрет ok=true");
                    break;
                }
                dbg_log("warmup: тестовая транскрипция не удалась, пробуем fallback");
            }
            Err(error) => {
                if ctx.recording.load(Ordering::Acquire) {
                    dbg_log("warmup: отменён начавшейся диктовкой");
                    break;
                }
                dbg_log(&format!(
                    "warmup: runtime {runtime:?} ОШИБКА: {error:#}; пробуем fallback"
                ));
                disable_accelerated_runtime(&ctx, runtime, &error);
            }
        }
    }
}

fn engine_loop(rx: Receiver<EngineCmd>, ctx: EngineCtx) {
    // Capture (cpal Stream) создаётся и уничтожается только здесь — он !Send.
    let mut capture: Option<Capture> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            EngineCmd::Start => start_capture_into(&mut capture, &ctx, false),
            EngineCmd::StartLatched => {
                start_capture_into(&mut capture, &ctx, true);
            }
            EngineCmd::Stop => stop_and_process(&mut capture, &ctx),
            EngineCmd::StopTap => stop_tap_and_process(&mut capture, &ctx),
            EngineCmd::Toggle => {
                if capture.is_some() {
                    stop_and_process(&mut capture, &ctx);
                } else {
                    start_capture_into(&mut capture, &ctx, false);
                }
            }
            EngineCmd::Cancel => cancel_current(&mut capture, &ctx),
            EngineCmd::ImproveSelection => improve_selection(&ctx),
            EngineCmd::ManualEdit => note_manual_edit(&ctx),
            EngineCmd::Warmup => {
                // В отдельном потоке: warmup сам спит и грузит модели —
                // канал команд блокировать нельзя (Start/Stop должны жить).
                let wctx = ctx.clone();
                std::thread::spawn(move || warmup(wctx));
            }
            EngineCmd::Shutdown => {
                let _activity = ctx.cancel_activity_lock.lock();
                ctx.cancel_active.store(false, Ordering::SeqCst);
                close_correction_capture(&ctx);
                restore_auto_mute(&ctx);
                if let Some(mut srv) = ctx.server.lock().take() {
                    let _ = srv.child.kill();
                }
                break;
            }
        }
    }
}

fn notify_hotkey_latch(ctx: &EngineCtx) {
    if ctx.settings.lock().play_sounds {
        sound::latch();
    }
    let _ = ctx.app.emit(
        "hotkey_latch",
        serde_json::json!({
            "message": "Без удержания",
            "detail": "Двойное нажатие"
        }),
    );
    dbg_log("hotkey: double-press latch enabled");
}

#[cfg(target_os = "macos")]
fn macos_insertion_preflight(ctx: &EngineCtx) -> bool {
    let allowed = crate::macos_permissions::post_event_allowed();
    if !insertion_permission_blocks_capture(true, allowed) {
        return true;
    }
    dbg_log("start: Accessibility/Post Event missing — capture blocked before microphone open");
    crate::macos_permissions::request_post_event_once();
    crate::macos_permissions::open_accessibility_settings();
    emit_error(
        &ctx.app,
        "Разрешите VoxFlow в macOS Privacy & Security -> Accessibility для вставки текста, затем повторите диктовку",
    );
    set_status(ctx, "idle");
    false
}

fn start_capture_into(capture: &mut Option<Capture>, ctx: &EngineCtx, latched: bool) {
    if capture.is_some() {
        return;
    }
    // A new dictation ends the previous correction-learning opportunity before
    // its hotkey can be mistaken for an edit of the last inserted sentence.
    close_correction_capture(ctx);
    // Новое физическое нажатие атомарно забирает ownership старого живого
    // черновика и инвалидирует detached-финал БЕЗ platform context lookup.
    // Само безопасное стирание выполняется уже после открытия микрофона: на
    // macOS Accessibility может отвечать 150–650 мс, и ожидание до CoreAudio
    // обрезало бы начало короткой фразы.
    let (start_gen, inherited_live_draft) = advance_generation_for_start(ctx);
    // Визуальный отклик также не ждёт CoreAudio/platform context. Overlay не получает
    // фокус, поэтому цель всё равно снимается с внешнего приложения ниже.
    set_status_with_latch(ctx, "recording", latched);
    if latched {
        // The generation-aware status above atomically prevents a rec→latch
        // flash. Keep the dedicated event for its confirmation sound/message.
        notify_hotkey_latch(ctx);
    }
    #[cfg(target_os = "macos")]
    if !macos_insertion_preflight(ctx) {
        erase_claimed_live_draft_with_fresh_context(ctx, inherited_live_draft.as_ref(), start_gen);
        *ctx.active_target.lock() = None;
        clear_cancel_active_if_current(ctx, start_gen);
        return;
    }
    let (device, play, auto_mute) = {
        let s = ctx.settings.lock();
        (s.input_device.clone(), s.play_sounds, s.auto_mute)
    };
    match audio::start_capture(&device) {
        Ok(c) => {
            // Открываем CoreAudio ДО гарда модели. На macOS именно этот
            // первый доступ запускает TCC-запрос микрофона. На чистой
            // установке модель ещё может скачиваться; если проверить её
            // раньше, до микрофона код не доходит и macOS не показывает prompt.
            // Поток тут же дропается, если локальный ASR пока не готов.
            {
                let s = ctx.settings.lock();
                // Облако «активно» только при наличии ключа — иначе провайдер
                // openai_compat/deepgram de-facto уходит в локальное распознавание.
                let use_cloud_stt = s.cloud_stt_active();
                let use_cloud_gemini = s.cloud_asr
                    && s.ai_backend == "gemini"
                    && crate::gemini::available(&s.ai_api_key);
                let use_cloud = use_cloud_stt || use_cloud_gemini;
                if !use_cloud && no_model_installed(&s) {
                    drop(s);
                    drop(c);
                    erase_claimed_live_draft_with_fresh_context(
                        ctx,
                        inherited_live_draft.as_ref(),
                        start_gen,
                    );
                    *ctx.active_target.lock() = None;
                    dbg_log("start: модель не установлена — запись не начинаем, предупреждаем");
                    emit_no_model(&ctx.app);
                    set_status(ctx, "idle");
                    clear_cancel_active_if_current(ctx, start_gen);
                    return;
                }
            }
            // Capture is already running while we resolve the target. Target
            // discovery can synchronously consult macOS accessibility APIs, so
            // doing it before opening CoreAudio clipped the beginning of short
            // utterances. It still happens before every status/overlay event,
            // which preserves the original anti-focus-steal guarantee.
            let detected_context = detect_current_context();
            let mut target_fp = resolve_start_target_from_context(ctx, &detected_context);
            // The microphone is already recording. If the previous detached
            // final left a physical partial in this same field, remove exactly
            // that ledger entry before any partial loop for the new generation
            // can emit keys. Refresh the snapshot only when text was erased so
            // correction learning sees the actual post-cleanup field contents.
            if erase_claimed_live_draft_locked_to_generation(
                ctx,
                inherited_live_draft.as_ref(),
                start_gen,
                &detected_context,
            ) {
                let refreshed_context = detect_current_context();
                target_fp = resolve_start_target_from_context(ctx, &refreshed_context);
            }
            dbg_log(&format!("start: target {}", target_fp.describe()));
            *ctx.active_target.lock() = Some(target_fp.clone());
            // Пояс безопасности (C2): старт-звук играем ТОЛЬКО на честном переходе
            // «не писали → пишем». Если запись уже шла (Start поверх ещё активной
            // диктовки при rapid-fire) — звук не переигрываем.
            let was_recording = ctx.recording.swap(true, Ordering::SeqCst);
            if auto_mute && !was_recording {
                match AutoMuteGuard::engage() {
                    Ok(guard) => {
                        *ctx.auto_mute.lock() = Some(guard);
                        dbg_log("auto-mute: system output muted for dictation");
                    }
                    Err(e) => log::warn!("auto-mute engage failed: {e:#}"),
                }
            }
            if play && !was_recording {
                sound::play(true);
            }
            // Запускаем петлю живого стриминга, если GPU whisper-server доступен —
            // пилюля показывает живой текст и при whisper_cli (живой инжект при этом
            // выключен, см. maybe_start_partial_loop). Без GPU/модели — статичное «Слушаю…».
            maybe_start_partial_loop(&c, ctx, &target_fp);
            // Петля уровня громкости для orb-визуализатора (событие "level").
            spawn_level_loop(&c, ctx);
            *capture = Some(c);
        }
        Err(err) => {
            erase_claimed_live_draft_with_fresh_context(
                ctx,
                inherited_live_draft.as_ref(),
                start_gen,
            );
            *ctx.active_target.lock() = None;
            log::error!("start_capture: {err:#}");
            emit_error(&ctx.app, &format!("Не удалось открыть микрофон: {err}"));
            set_status(ctx, "idle");
            clear_cancel_active_if_current(ctx, start_gen);
        }
    }
}

/// Поднять петлю живых частичных результатов, если whisper-server физически
/// способен их дать (есть GPU и резолвится модель) — НЕЗАВИСИМО от выбранного
/// «движка финала». Так живая пилюля работает и при engine==whisper_cli (финал
/// всё равно пойдёт через cli), и при whisper_server.
///
/// ВАЖНО: для cli живой ИНЖЕКТ клавишами не нужен/опасен, поэтому stream_mode
/// для петли при cli трактуем как "never" — показываем только серый текст в пилюле.
fn whisper_preview_requested(stream_mode: &str) -> bool {
    // Preview-only (`never`) is intentionally supported by the already-resident
    // GigaAM/Parakeet routes below: it emits text to the overlay but never types
    // into the target field. Do not start the heavier Whisper sidecar in that
    // mode, because it can still contend with final ASR.
    stream_mode != "never"
}

fn maybe_start_partial_loop(capture: &Capture, ctx: &EngineCtx, target_fp: &TargetFingerprint) {
    // Сбрасываем прошлое состояние на всякий случай (новая диктовка = чистый старт).
    *ctx.partial.lock() = None;

    let s = ctx.settings.lock().clone();
    // ГИБРИД (бриф: «локальный мгновенный черновик → точный облачный финал»):
    // если выбран облачный STT и ключ ЕСТЬ (cloud_active), пилюлю НЕ глушим — наоборот,
    // крутим локальный сервер живого черновика, чтобы показать МГНОВЕННЫЙ серый ЧЕРНОВИК в пилюле;
    // в поле при этом ничего не печатаем (effective_mode → "never" ниже), потому что точный
    // финал придёт из облака и вставится один раз. Если ключа нет — мы de-facto работаем
    // локально, поведение как у "local" (умный фолбэк, решение пользователя).
    let cloud_active = s.cloud_stt_active();
    // ОБЛАЧНЫЙ живой черновик: если STT — облако с ключом, локальный GPU/модель
    // НЕ нужны. Шлём растущий буфер прямо в облако (Groq/Avalon/Deepgram) каждые ~1.4с →
    // серый текст в пилюле, «как у офлайн-моделей», но через API-ключ. В поле НЕ печатаем
    // (точный финал вставится один раз). Это и УБИРАЕТ ложный наг «выберите модель»:
    // раньше гибрид пытался поднять локальный сервер ради черновика и при отсутствии
    // модели слал no_model, хотя облако для распознавания полностью рабочее.
    if cloud_active {
        if s.cloud_live_draft {
            start_cloud_partial_loop(capture, ctx, &s, target_fp);
        } else {
            dbg_log("partial: облако активно, живой черновик выключен — пилюля статична");
        }
        return;
    }
    // Облачный ASR (Gemini) не даёт живых партиалов через whisper-server — пропускаем.
    let use_cloud =
        s.cloud_asr && s.ai_backend == "gemini" && crate::gemini::available(&s.ai_api_key);
    if use_cloud {
        return; // облачный путь: без живых частичных результатов.
    }
    // Локальные резидентные движки: живые партиалы на CPU, GPU не нужен.
    // Сегментная схема: VAD находит паузы; завершённые сегменты фиксируются
    // (committed растёт монотонно по построению), активный сегмент
    // перераспознаётся каждый тик (volatile, серый). По тишине ASR не гоняем.
    if s.language == "auto" && s.engine == "gigaam" {
        if resident_model_ready(&ctx.gigaam) {
            // Auto сохраняет все языки в финале, но русскому live-preview нужен
            // быстрый и сильный движок. Whisper auto слишком медленно обновлял
            // кружок, а Parakeet давал мусор по русской речи. Поэтому в auto
            // показываем быстрый GigaAM-preview только в кружке (без live-вставки).
            let mut preview_settings = s.clone();
            preview_settings.stream_mode = "never".into();
            start_local_partial_loop(
                capture,
                ctx,
                &preview_settings,
                target_fp,
                Arc::clone(&ctx.gigaam),
                LocalLoopTuning {
                    tick_ms: 220,
                    max_seg_samples: 8 * 16000,
                    fixed_lang: Some("ru"),
                },
            );
            return;
        }
        dbg_log("partial: auto+gigaam preview недоступен — пробуем whisper-стрим");
    }
    #[cfg(target_os = "macos")]
    if s.engine == "whisper_server"
        && (s.language.eq_ignore_ascii_case("ru") || is_auto_language_alias(&s.language))
        && crate::gigaam::dir_ready(&paths::gigaam_dir())
        && resident_model_ready(&ctx.gigaam)
    {
        // macOS UX: универсальный Whisper large слишком медленный для живых
        // partial-ов на CPU/Metal и часто упирается в короткий live timeout.
        // Если русский GigaAM уже установлен, используем его как быстрый
        // preview только в плашке; финальный движок остаётся выбранным в UI.
        let mut preview_settings = s.clone();
        preview_settings.stream_mode = "never".into();
        dbg_log("partial: macOS fast GigaAM preview enabled");
        start_local_partial_loop(
            capture,
            ctx,
            &preview_settings,
            target_fp,
            Arc::clone(&ctx.gigaam),
            LocalLoopTuning {
                tick_ms: 260,
                max_seg_samples: 8 * 16000,
                fixed_lang: Some("ru"),
            },
        );
        return;
    }
    match local_route(&s) {
        LocalRoute::GigaAm => {
            if !resident_model_ready(&ctx.gigaam) {
                // Модели ещё нет (первый запуск, докачка) — пилюля статична,
                // предупреждение по-старому отработает финал/гард старта.
                dbg_log("partial: gigaam не готов — без живого стрима");
                return;
            }
            // GigaAM-маршрут = заведомо русский → фиксированный бейдж "ru".
            start_local_partial_loop(
                capture,
                ctx,
                &s,
                target_fp,
                Arc::clone(&ctx.gigaam),
                LocalLoopTuning {
                    tick_ms: 220,
                    max_seg_samples: 8 * 16000,
                    fixed_lang: Some("ru"),
                },
            );
            return;
        }
        LocalRoute::Parakeet => {
            if resident_model_ready(&ctx.parakeet) {
                // en/auto: партиалы гонит Parakeet БЕЗ двойного прогона (RU-перегон
                // кириллических сегментов — только в финале); язык бейджа
                // определяется по скрипту текущего текста.
                start_local_partial_loop(
                    capture,
                    ctx,
                    &s,
                    target_fp,
                    Arc::clone(&ctx.parakeet),
                    LocalLoopTuning {
                        tick_ms: 500,
                        max_seg_samples: 20 * 16000,
                        fixed_lang: None,
                    },
                );
                return;
            }
            // Файлы есть (route выбрался), но загрузка не удалась — падаем в
            // whisper-петлю ниже, как раньше.
            dbg_log("partial: parakeet не загрузился — пробуем whisper-стрим");
        }
        LocalRoute::Whisper => {}
    }
    if !whisper_preview_requested(&s.stream_mode) {
        dbg_log("partial: preview-only mode has no resident local route — whisper preview skipped");
        return;
    }
    // Живой whisper-стрим на Windows оставляем только для NVIDIA-сборки: CPU
    // сервер там слишком медленный для тиков. На macOS используем native sidecar
    // (Metal/CPU whisper.cpp), поэтому отсутствие NVIDIA не должно гасить overlay.
    let whisper_live_supported = cfg!(target_os = "macos") || paths::has_nvidia();
    if !whisper_live_supported {
        dbg_log("partial: нет GPU whisper-server — без живого стрима (пилюля статична)");
        return;
    }
    if cfg!(windows) && ctx.whisper_accelerated_disabled.load(Ordering::Acquire) {
        dbg_log("partial: CUDA runtime disabled — CPU fallback остаётся для финала");
        return;
    }
    #[cfg(target_os = "macos")]
    dbg_log("partial: macOS whisper-server live draft enabled");

    // Модель и runtime резолвим синхронно, но сам сервер поднимаем
    // в partial-потоке. Иначе несовместимый CUDA runtime блокирует очередь
    // EngineCmd и Stop не обрабатывается до истечения startup timeout.
    let whisper_dir = whisper_runtime_dirs(ctx)
        .into_iter()
        .next()
        .unwrap_or_else(|| paths::whisper_cpu_dir(&ctx.app));
    let model = match resolve_model(&s) {
        Ok(m) => m,
        Err(e) => {
            dbg_log(&format!(
                "partial: resolve_model ошибка: {e} — без стриминга"
            ));
            // B3: модели нет — предупреждаем пользователя сразу (а не молчим).
            if e.downcast_ref::<ModelMissing>().is_some() {
                emit_no_model(&ctx.app);
            }
            return;
        }
    };
    // Для cli живой инжект клавишами не выполняем — только показ серого текста в пилюле.
    // При активном облаке (cloud_active) живой инжект тоже выключаем: пилюля показывает
    // черновик, а в поле вставляется только точный облачный финал (один раз).
    let effective_mode = if cloud_active {
        "never".to_string()
    } else if s.engine == "whisper_server" {
        s.stream_mode.clone()
    } else {
        "never".to_string()
    };

    // Отпечаток целевого окна на старте — для защиты от смены приложения.
    let start_fp = target_fp.clone();
    // Поколение этой диктовки — суффикс seq для событий partial (фронт отбрасывает
    // устаревшие партиалы прошлой диктовки при гонке доставки/StrictMode-листенерах).
    let my_seq = ctx.gen.load(Ordering::SeqCst);

    let stop = Arc::new(AtomicBool::new(false));
    let abort = Arc::new(AtomicBool::new(false));
    let injected = Arc::new(Mutex::new(String::new()));
    let committed = Arc::new(Mutex::new(String::new()));

    let rate = capture.sample_rate();
    let buf = capture.buffer_handle();
    let app = ctx.app.clone();
    let asr_lock = Arc::clone(&ctx.asr_lock);
    let inject_lock = Arc::clone(&ctx.inject_lock);
    let lang = whisper_language_arg(&s.language);
    let mode = effective_mode;
    let settings = s.clone();
    let (dict, snippets, corrections) = load_live_postprocess_data(ctx);

    // Клоны Arc для потока (originals остаются в PartialState).
    let t_stop = Arc::clone(&stop);
    let server_cancel = Arc::clone(&stop);
    let t_abort = Arc::clone(&abort);
    let t_injected = Arc::clone(&injected);
    let t_committed = Arc::clone(&committed);
    let t_fp = start_fp.clone();
    let t_mode = mode.clone();
    let server_ctx = ctx.clone();
    let server_threads = s.effective_threads();

    let join = std::thread::Builder::new()
        .name("voxflow-partial".into())
        .spawn(move || {
            let port = match ensure_server_cancellable(
                &server_ctx,
                &whisper_dir,
                &model,
                server_threads,
                server_cancel.as_ref(),
            ) {
                Ok(port) if !server_cancel.load(Ordering::Acquire) => port,
                Ok(_) => return,
                Err(error) => {
                    if !server_cancel.load(Ordering::Acquire) {
                        dbg_log(&format!(
                            "partial: ensure_server ошибка: {error:#} — без стриминга"
                        ));
                        disable_accelerated_runtime(&server_ctx, &whisper_dir, &error);
                    }
                    return;
                }
            };
            partial_loop(PartialLoopArgs {
                buffer: buf,
                rate,
                app,
                port,
                language: lang,
                asr_lock,
                inject_lock,
                stop: t_stop,
                abort: t_abort,
                injected: t_injected,
                committed: t_committed,
                start_fp: t_fp,
                stream_mode: t_mode,
                seq: my_seq,
                settings,
                dict,
                snippets,
                corrections,
            });
        })
        .ok();

    *ctx.partial.lock() = Some(PartialState {
        stop,
        join,
        injected,
        committed,
        abort,
        start_fp,
        stream_mode: mode,
        kind: PartialKind::WhisperServer,
    });
}

/// Максимум облачных черновиков на ОДНУ диктовку (бюджет API-квоты). После — только
/// финал; пилюля держит последний показанный черновик. Намеренно НИЗКИЙ: каждый тик
/// заново транскрибирует РАСТУЩИЙ буфер (аудио-секунды накапливаются), а бесплатные
/// тиры (Groq) ограничены по аудио-секундам — поэтому пара-тройка превью на диктовку,
/// а не непрерывный поток, чтобы не сжечь квоту, нужную самому финалу.
const CLOUD_DRAFT_CAP: u32 = 4;

/// Запустить ОБЛАЧНЫЙ живой черновик (Groq/Avalon/Deepgram) для пилюли. Локальный
/// whisper/GPU/модель НЕ нужны — шлём растущий буфер прямо в облако.
///
/// ВАЖНО (UX): поток ДЕТАЧИМ (`join: None`). Иначе `stop_and_process` на отпускании
/// клавиши заблокировался бы до завершения текущего сетевого запроса (~1–2с) и финал
/// ощущался бы лагающим. Детач безопасен: петля само-ограничена (`stop` + CAP), эмитит
/// `partial` ТОЛЬКО пока `stop`==false (проверка перед эмиссией), пишет в собственный
/// WAV (имя по seq, без гонки с соседней диктовкой) и сама за собой убирает.
fn start_cloud_partial_loop(
    capture: &Capture,
    ctx: &EngineCtx,
    s: &Settings,
    target_fp: &TargetFingerprint,
) {
    let start_fp = target_fp.clone();
    let my_seq = ctx.gen.load(Ordering::SeqCst);

    let stop = Arc::new(AtomicBool::new(false));
    let rate = capture.sample_rate();
    let buf = capture.buffer_handle();
    let app = ctx.app.clone();
    let s_clone = s.clone();
    let (dict, snippets, corrections) = load_live_postprocess_data(ctx);
    let t_stop = Arc::clone(&stop);

    // Детачим (handle не сохраняем) — stop_and_process не будет ждать сетевой запрос.
    let _ = std::thread::Builder::new()
        .name("voxflow-cloud-partial".into())
        .spawn(move || {
            cloud_partial_loop(
                buf,
                rate,
                app,
                s_clone,
                dict,
                snippets,
                corrections,
                t_stop,
                my_seq,
            )
        });

    // stream_mode="never" + пустые injected/committed/abort: финал пойдёт обычной
    // одиночной облачной вставкой, реконсиляция клавишами не выполняется. join=None →
    // stop_and_process только выставит stop (петля сама завершится), но НЕ заблокируется.
    *ctx.partial.lock() = Some(PartialState {
        stop,
        join: None,
        injected: Arc::new(Mutex::new(String::new())),
        committed: Arc::new(Mutex::new(String::new())),
        abort: Arc::new(AtomicBool::new(false)),
        start_fp,
        stream_mode: "never".to_string(),
        kind: PartialKind::Cloud,
    });
}

/// Тело облачной петли: каждые ~1.4с шлёт растущий буфер в облако и эмитит "partial".
/// Бюджет API: тик только при ≥1с НОВОГО звука, не более [`CLOUD_DRAFT_CAP`] запросов;
/// ошибки/429/таймаут — best-effort (пропуск тика). Эмиссия только пока stop==false.
#[allow(clippy::too_many_arguments)]
fn cloud_partial_loop(
    buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    rate: u32,
    app: AppHandle,
    s: Settings,
    dict: Vec<postprocess::Dict>,
    snippets: Vec<postprocess::Snippet>,
    corrections: Vec<postprocess::Correction>,
    stop: Arc<AtomicBool>,
    seq: u64,
) {
    let min_new16 = 16000usize; // ≥1с НОВОГО звука (в 16к-домене) на тик — экономим запросы
    let mut last_len16 = 0usize;
    let mut sent = 0u32;
    let mut idle = 0u32; // тиков подряд без нового звука (тишина/пауза)
    let mut stab = PrefixStabilizer::new(4, 2);
    // P1-1: снимаем только хвост буфера (tail_since) и ресемплим инкрементально —
    // полный clone + ре-ресемпл растущего буфера каждый тик блокировал data-callback.
    let mut cursor = 0usize;
    let mut rs = audio::Resampler16k::new(rate);
    let mut mono16: Vec<f32> = Vec::new();
    let wav = paths::tmp_dir().join(format!("cloud_partial_{seq}.wav"));
    loop {
        std::thread::sleep(Duration::from_millis(2000)); // коарс-каденс (бюджет API)
        if stop.load(Ordering::Acquire) || sent >= CLOUD_DRAFT_CAP {
            break;
        }
        let (tail, ncur) = audio::tail_since(&buffer, cursor);
        cursor = ncur;
        mono16.extend(rs.feed(&tail));
        // Нужно ≥1с нового звука с прошлого УСПЕШНОГО тика.
        if mono16.len() < last_len16 + min_new16 {
            // Нет нового звука — копим тишину. ~3 тика (≈6с) тишины → глушим
            // петлю заранее (не жжём поток и сетевые запросы во время паузы),
            // не дожидаясь stop от отпускания клавиши.
            idle += 1;
            if idle >= 3 {
                break;
            }
            continue;
        }
        idle = 0;
        let trimmed = audio::trim_silence(&mono16, 16000);
        if trimmed.len() < 16000 {
            continue; // <1с полезного звука — рано
        }
        if audio::write_wav_16k_mono(&wav, &trimmed).is_err() {
            continue;
        }
        // Сетевой вызов ~1–2с: проверяем stop ДО (вдруг уже отпустили) и ПОСЛЕ (чтобы
        // не показать черновик поверх уже идущего финала).
        if stop.load(Ordering::Acquire) {
            break;
        }
        let text = match crate::cloud_stt::transcribe_partial(&s, &wav) {
            Ok(t) => t,
            Err(_) => continue, // 429/таймаут/сеть — best-effort, пропускаем тик
        };
        if stop.load(Ordering::Acquire) {
            break;
        }
        last_len16 = mono16.len();
        sent += 1;
        if text.trim().is_empty() {
            continue;
        }
        let (committed_raw, volatile_raw) = stab.push(&text);
        let (committed, volatile, full) = clean_live_partial(
            &committed_raw,
            &volatile_raw,
            &s,
            &dict,
            &snippets,
            &corrections,
        );
        if full.is_empty() {
            continue;
        }
        // Третья проверка stop — прямо перед эмиссией: закрываем узкое TOCTOU-окно,
        // чтобы НЕ показать черновик ПОВЕРХ уже идущего финала (отпустили клавишу).
        if stop.load(Ordering::Acquire) {
            break;
        }
        let _ = app.emit(
            "partial",
            live_partial_payload(&full, &committed, &volatile, seq, None),
        );
    }
    let _ = std::fs::remove_file(&wav);
}

/// Сегменты надиктованного: (абзац-перед?, текст). Рендер: " " либо "\n\n".
fn render_segments(segs: &[(bool, String)]) -> String {
    let mut out = String::new();
    for (i, (para, t)) in segs.iter().enumerate() {
        if i > 0 {
            out.push_str(if *para { "\n\n" } else { " " });
        }
        out.push_str(t);
    }
    out
}

fn push_dictation_segment(segs: &mut Vec<(bool, String)>, para: bool, text: String) {
    let text = text.trim().to_string();
    if text.is_empty() {
        return;
    }
    if let Some(last) = segs.last_mut() {
        if is_restatement(&last.1, &text) {
            last.1 = text;
            return;
        }
        if !para && soft_join_continuation_segment(&mut last.1, &text) {
            return;
        }
    }
    segs.push((para, text));
}

fn soft_join_continuation_segment(prev: &mut String, next: &str) -> bool {
    let Some(next_clean) = lower_if_continuation_start(next) else {
        return false;
    };
    let trimmed = prev.trim_end();
    if !trimmed.ends_with('.') && !trimmed.ends_with('…') {
        return false;
    }
    while prev.ends_with(char::is_whitespace) {
        prev.pop();
    }
    while prev.ends_with('.') {
        prev.pop();
    }
    if prev.ends_with('…') {
        prev.pop();
    }
    while prev.ends_with(char::is_whitespace) {
        prev.pop();
    }
    prev.push_str(", ");
    prev.push_str(&next_clean);
    true
}

fn lower_if_continuation_start(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !starts_with_continuation_cue(trimmed) {
        return None;
    }
    Some(lower_first_alphabetic(trimmed))
}

fn starts_with_continuation_cue(text: &str) -> bool {
    const CUES: &[&str] = &[
        "то есть",
        "потому что",
        "а",
        "и",
        "но",
        "чтобы",
        "если",
        "когда",
        "который",
        "которая",
        "которое",
        "которые",
        "поэтому",
        "наверное",
        "просто",
        "ещё",
        "видишь",
        "допустим",
    ];
    let lower = text.to_lowercase();
    CUES.iter().any(|cue| {
        if !lower.starts_with(cue) {
            return false;
        }
        lower[cue.len()..]
            .chars()
            .next()
            .map(|c| !c.is_alphanumeric())
            .unwrap_or(true)
    })
}

fn lower_first_alphabetic(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut lowered = false;
    for ch in text.chars() {
        if !lowered && ch.is_alphabetic() {
            out.extend(ch.to_lowercase());
            lowered = true;
        } else {
            out.push(ch);
        }
    }
    out
}

fn load_live_postprocess_data(
    ctx: &EngineCtx,
) -> (
    Vec<postprocess::Dict>,
    Vec<postprocess::Snippet>,
    Vec<postprocess::Correction>,
) {
    let conn = ctx.db.lock();
    (
        load_dict(&conn),
        load_snippets(&conn),
        load_corrections(&conn),
    )
}

fn clean_live_text(
    text: &str,
    s: &Settings,
    dict: &[postprocess::Dict],
    snippets: &[postprocess::Snippet],
    corrections: &[postprocess::Correction],
) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    // Exact snippet bodies are authored output, not an ASR hypothesis. Keep the
    // live pill byte-for-byte aligned with the final insertion: corrections,
    // capitalization and whitespace normalization must not rewrite the body.
    if let Some(expanded) = postprocess::expand_matching_snippet(text, snippets) {
        return expanded;
    }
    let mut t = postprocess::process(text, s, dict, snippets);
    t = postprocess::apply_corrections(&t, corrections);
    postprocess::normalize_spaces(&t)
}

fn join_partial(committed: &str, volatile: &str) -> String {
    match (committed.is_empty(), volatile.is_empty()) {
        (false, false) => {
            if committed.ends_with('\n') {
                format!("{committed}{volatile}")
            } else {
                format!("{committed} {volatile}")
            }
        }
        (false, true) => committed.to_string(),
        (true, false) => volatile.to_string(),
        (true, true) => String::new(),
    }
}

fn clean_live_partial(
    committed: &str,
    volatile: &str,
    s: &Settings,
    dict: &[postprocess::Dict],
    snippets: &[postprocess::Snippet],
    corrections: &[postprocess::Correction],
) -> (String, String, String) {
    let committed_clean = clean_live_text(committed, s, dict, snippets, corrections);
    let full_raw = join_partial(committed, volatile);
    let full = clean_live_text(&full_raw, s, dict, snippets, corrections);
    if full.is_empty() {
        return (String::new(), String::new(), String::new());
    }
    if committed.trim().is_empty() {
        return (String::new(), full.clone(), full);
    }
    if volatile.trim().is_empty() {
        return (full.clone(), String::new(), full);
    }
    if !committed_clean.is_empty() {
        if let Some(rest) = full.strip_prefix(&committed_clean) {
            return (committed_clean, rest.trim_start().to_string(), full);
        }
    }
    (String::new(), full.clone(), full)
}

/// Похоже ли `b` на переговорённую заново версию `a` (человек ошибся, сделал
/// паузу и сказал фразу ещё раз): пословный Жаккар >= 0.5 при сопоставимой
/// длине. Сравниваем только СОСЕДНИЕ сегменты — типичный паттерн самоправки.
fn is_restatement(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(|w| {
                w.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect()
    };
    let ta = norm(a);
    let tb = norm(b);
    if ta.len() < 2 || tb.len() < 2 {
        return false;
    }
    let (la, lb) = (ta.len() as f64, tb.len() as f64);
    if lb < la * 0.5 || lb > la * 2.5 {
        return false;
    }
    let sa: HashSet<&String> = ta.iter().collect();
    let sb: HashSet<&String> = tb.iter().collect();
    let inter = sa.intersection(&sb).count() as f64;
    let union = sa.union(&sb).count() as f64;
    inter / union.max(1.0) >= 0.5
}

/// Для вставки КЛАВИШАМИ (живые режимы) абзацы заменяем пробелом: Enter в
/// чатах отправил бы сообщение. Абзацы доезжают только в clipboard-финале.
fn flatten_breaks(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            ws = true;
            continue;
        }
        if ws && !out.is_empty() {
            out.push(' ');
        }
        ws = false;
        out.push(ch);
    }
    out
}

/// Лёгкая петля уровня громкости для orb-визуализатора: каждые ~33 мс снимает
/// хвост буфера (~50 мс), считает RMS → нормирует в 0..1 → событие "level".
/// Живёт, пока recording==true; на выходе шлёт нулевой уровень (бары опадают).
fn spawn_level_loop(capture: &Capture, ctx: &EngineCtx) {
    let buf = capture.buffer_handle();
    let rate = capture.sample_rate() as usize;
    let app = ctx.app.clone();
    let recording = Arc::clone(&ctx.recording);
    let gen = Arc::clone(&ctx.gen);
    let seq = ctx.gen.load(Ordering::SeqCst);
    let _ = std::thread::Builder::new()
        .name("voxflow-level".into())
        .spawn(move || {
            let win = (rate / 20).max(1); // окно RMS ~50 мс
                                          // Гард по gen (P2-3): при rapid-fire рестарте recording может остаться true
                                          // для УЖЕ НОВОЙ диктовки — осиротевшая петля прошлого поколения обязана
                                          // погаснуть сама, а не жить параллельно (потоки/IPC копились).
            while recording.load(Ordering::SeqCst) && gen.load(Ordering::SeqCst) == seq {
                let rms = match buf.lock() {
                    Ok(g) if !g.is_empty() => {
                        let s = &g[g.len().saturating_sub(win)..];
                        (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt()
                    }
                    _ => 0.0,
                };
                // Перцептивная нормировка: тихая речь ~0.01 RMS, громкая ~0.2.
                let v = (rms / 0.18).powf(0.5).clamp(0.0, 1.0);
                let _ = app.emit("level", serde_json::json!({ "rms": v, "seq": seq }));
                std::thread::sleep(Duration::from_millis(33));
            }
            let _ = app.emit("level", serde_json::json!({ "rms": 0.0, "seq": seq }));
        });
}

/// Каденс/лимиты петли локальных партиалов: GigaAM — быстрый первый тик 220 мс
/// и кап сегмента 8 c, Parakeet — тик 500 мс и кап 20 c.
struct LocalLoopTuning {
    tick_ms: u64,
    max_seg_samples: usize,
    /// Some("ru") — бейдж языка фиксирован (GigaAM-маршрут);
    /// None — определяется по скрипту текущего текста (Parakeet en/auto).
    fixed_lang: Option<&'static str>,
}

/// Аргументы петли живых партиалов локального резидентного движка
/// (GigaAM/Parakeet; CPU, сегментная схема по VAD-паузам).
struct LocalLoopArgs<T: LocalStt> {
    buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    rate: u32,
    app: AppHandle,
    engine: Arc<Mutex<Option<T>>>,
    vad: Arc<Mutex<Option<crate::vad::SileroVad>>>,
    inject_lock: Arc<Mutex<()>>,
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    injected: Arc<Mutex<String>>,
    committed_field: Arc<Mutex<String>>,
    start_fp: TargetFingerprint,
    stream_mode: String,
    seq: u64,
    tuning: LocalLoopTuning,
    settings: Settings,
    dict: Vec<postprocess::Dict>,
    snippets: Vec<postprocess::Snippet>,
    corrections: Vec<postprocess::Correction>,
}

/// Поднять петлю живых партиалов локального движка. stream_mode действует как
/// у whisper: never — только пилюля, auto — committed в поле, always — всё в поле.
fn start_local_partial_loop<T: LocalStt + Send + 'static>(
    capture: &Capture,
    ctx: &EngineCtx,
    s: &Settings,
    target_fp: &TargetFingerprint,
    engine: Arc<Mutex<Option<T>>>,
    tuning: LocalLoopTuning,
) {
    let start_fp = target_fp.clone();
    let my_seq = ctx.gen.load(Ordering::SeqCst);

    let stop = Arc::new(AtomicBool::new(false));
    let abort = Arc::new(AtomicBool::new(false));
    let injected = Arc::new(Mutex::new(String::new()));
    let committed = Arc::new(Mutex::new(String::new()));
    let (dict, snippets, corrections) = load_live_postprocess_data(ctx);

    let args = LocalLoopArgs {
        buffer: capture.buffer_handle(),
        rate: capture.sample_rate(),
        app: ctx.app.clone(),
        engine,
        vad: Arc::clone(&ctx.vad),
        inject_lock: Arc::clone(&ctx.inject_lock),
        stop: Arc::clone(&stop),
        abort: Arc::clone(&abort),
        injected: Arc::clone(&injected),
        committed_field: Arc::clone(&committed),
        start_fp: start_fp.clone(),
        stream_mode: s.stream_mode.clone(),
        seq: my_seq,
        tuning,
        settings: s.clone(),
        dict,
        snippets,
        corrections,
    };
    let join = std::thread::Builder::new()
        .name("voxflow-local-partial".into())
        .spawn(move || local_partial_loop(args))
        .ok();

    *ctx.partial.lock() = Some(PartialState {
        stop,
        join,
        injected,
        committed,
        abort,
        start_fp,
        stream_mode: s.stream_mode.clone(),
        kind: PartialKind::Local,
    });
}

/// Тело петли локальных партиалов (GigaAM/Parakeet). VAD стримово размечает
/// новые сэмплы; пауза ≥600 мс (или сегмент ≥ tuning.max_seg_samples) закрывает
/// активный сегмент: его текст один раз фиксируется в committed (больше НЕ
/// переписывается), дальше распознаётся только новый активный кусок. Тишину
/// не распознаём.
fn local_partial_loop<T: LocalStt>(a: LocalLoopArgs<T>) {
    const SPEECH_PROB: f32 = 0.35;
    const SIL_BOUND_MS: usize = 600;
    const SETTLED_MS: usize = 3000;
    let tick_ms = a.tuning.tick_ms;
    let max_seg_samples = a.tuning.max_seg_samples;

    // Только длинная пауза создаёт новый абзац: короткая остановка чаще означает,
    // что пользователь продолжает ту же мысль.
    let mut committed_segs: Vec<(bool, String)> = Vec::new();
    let mut seg_start = 0usize; // оффсет активного сегмента (16к-домен)
    let mut vad_pos = 0usize; // докуда прогнали стриминговый VAD
    let mut last_speech_end = 0usize; // конец последнего речевого VAD-чанка
    let mut prev_seg_end = 0usize; // конец речи последнего ЗАКРЫТОГО сегмента
    let mut cur_seg_first_speech: Option<usize> = None;
    let mut seg_has_speech = false;
    let mut last_emitted: Option<(String, String)> = None;
    let mut settled_emitted_for_end = 0usize;

    // Свой стриминговый VAD-state на диктовку.
    if let Some(v) = a.vad.lock().as_mut() {
        v.reset();
    }

    // P1-1: вместо клона ВСЕГО буфера + полного ре-ресемпла каждый тик (O(n²)
    // за диктовку, лок на десятки мс → дропы сэмплов в cpal data-callback) —
    // снимаем только хвост (tail_since) и ресемплим инкрементально; mono16
    // только дописывается, так что все оффсеты (vad_pos/seg_start) стабильны.
    let mut cursor = 0usize;
    let mut rs = audio::Resampler16k::new(a.rate);
    let mut mono16: Vec<f32> = Vec::new();

    loop {
        // Fast first words, then lower the inference cadence for a long
        // uninterrupted phrase. Together with the 8 s segment cap this keeps
        // preview work bounded and leaves the shared resident model available
        // to final ASR soon after key-up.
        let active_samples = mono16.len().saturating_sub(seg_start);
        let cadence_ms = if tick_ms < 400 && active_samples >= 4 * 16000 {
            420
        } else {
            tick_ms
        };
        std::thread::sleep(Duration::from_millis(cadence_ms));
        if a.stop.load(Ordering::Acquire) {
            break;
        }
        let (tail, ncur) = audio::tail_since(&a.buffer, cursor);
        cursor = ncur;
        mono16.extend(rs.feed(&tail));
        if mono16.len() < vad_pos + crate::vad::CHUNK {
            continue;
        }
        // Стриминговый VAD только по НОВЫМ сэмплам — дёшево (≈0.14 мс на чанк).
        {
            let mut vguard = a.vad.lock();
            let Some(v) = vguard.as_mut() else { continue };
            while vad_pos + crate::vad::CHUNK <= mono16.len() {
                let p = v
                    .process_chunk(&mono16[vad_pos..vad_pos + crate::vad::CHUNK])
                    .unwrap_or(0.0);
                vad_pos += crate::vad::CHUNK;
                if p >= SPEECH_PROB {
                    if !seg_has_speech {
                        // Первый речевой чанк нового сегмента: длина паузы
                        // перед ним решит, начинать ли с него абзац.
                        cur_seg_first_speech = Some(vad_pos - crate::vad::CHUNK);
                    }
                    last_speech_end = vad_pos;
                    seg_has_speech = true;
                }
            }
        }
        if !seg_has_speech {
            let settled_silence = vad_pos.saturating_sub(prev_seg_end);
            if prev_seg_end > 0
                && !committed_segs.is_empty()
                && settled_silence >= SETTLED_MS * 16
                && settled_emitted_for_end != prev_seg_end
            {
                let (committed, volatile, full) = clean_live_partial(
                    &render_segments(&committed_segs),
                    "",
                    &a.settings,
                    &a.dict,
                    &a.snippets,
                    &a.corrections,
                );
                if !full.is_empty() && !a.stop.load(Ordering::Acquire) {
                    settled_emitted_for_end = prev_seg_end;
                    last_emitted = Some((committed.clone(), volatile.clone()));
                    if a.stream_mode == "never" {
                        *a.committed_field.lock() = full.clone();
                    }
                    let lang = a.tuning.fixed_lang.or_else(|| detect_lang_label(&full));
                    let _ = a.app.emit(
                        "partial",
                        settled_partial_payload(&full, &committed, a.seq, lang),
                    );
                }
            }
            continue; // в активном сегменте речи ещё нет — ASR не дёргаем
        }
        let silence_samples = vad_pos.saturating_sub(last_speech_end);
        let close_segment = silence_samples >= SIL_BOUND_MS * 16
            || mono16.len().saturating_sub(seg_start) >= max_seg_samples;

        // try_lock: финал уже забрал модель → тик пропускаем.
        let Some(mut g) = a.engine.try_lock() else {
            continue;
        };
        let Some(gm) = g.as_mut() else { continue };
        let (committed_raw, volatile_raw) = if close_segment {
            // Граница: последний речевой чанк + 300 мс хвоста.
            let bound = (last_speech_end + 4800).min(mono16.len());
            let txt = gm.transcribe(&mono16[seg_start..bound]).unwrap_or_default();
            drop(g);
            let t = txt.trim().to_string();
            if !t.is_empty() {
                // Длинная пауза перед сегментом -> абзац. Переговорённая заново
                // фраза ЗАМЕНЯЕТ предыдущий сегмент, а не дописывается дважды.
                let gap = cur_seg_first_speech
                    .unwrap_or(prev_seg_end)
                    .saturating_sub(prev_seg_end);
                let para = gap_starts_paragraph(!committed_segs.is_empty(), gap);
                push_dictation_segment(&mut committed_segs, para, t);
            }
            prev_seg_end = last_speech_end;
            seg_start = bound;
            seg_has_speech = false;
            cur_seg_first_speech = None;
            (render_segments(&committed_segs), String::new())
        } else {
            let txt = gm.transcribe(&mono16[seg_start..]).unwrap_or_default();
            drop(g);
            (render_segments(&committed_segs), txt.trim().to_string())
        };

        let (committed, volatile, full) = clean_live_partial(
            &committed_raw,
            &volatile_raw,
            &a.settings,
            &a.dict,
            &a.snippets,
            &a.corrections,
        );
        if full.is_empty() {
            continue;
        }
        if last_emitted.as_ref() == Some(&(committed.clone(), volatile.clone())) {
            continue; // ничего нового — не дёргаем фронт
        }
        last_emitted = Some((committed.clone(), volatile.clone()));
        if a.stop.load(Ordering::Acquire) {
            break; // не показываем партиал поверх идущего финала
        }
        if a.stream_mode == "never" {
            // В режиме «только плашка» храним показанный текст только для
            // UI/state; точный финал всегда берём из финального ASR.
            *a.committed_field.lock() = full.clone();
        }
        // Бейдж языка (контракт overlay): фиксированный "ru" у GigaAM-маршрута,
        // по скрипту текста у Parakeet (en/auto); null → бейдж скрыт.
        let lang = a.tuning.fixed_lang.or_else(|| detect_lang_label(&full));
        let _ = a.app.emit(
            "partial",
            live_partial_payload(&full, &committed, &volatile, a.seq, lang),
        );

        // Живая вставка в поле (always/auto) — как у whisper-петли.
        if a.abort.load(Ordering::Acquire) {
            continue;
        }
        match a.stream_mode.as_str() {
            "always" => {
                if !live_target_ok(&a.start_fp, &a.abort) {
                    continue;
                }
                let flat = flatten_breaks(&full);
                let _inj = a.inject_lock.lock();
                // Гонка осиротевшего тика (P2-5): финал выставляет stop ДО взятия
                // inject_lock — перепроверяем уже ПОД замком, чтобы детачнутый тик
                // не печатал поверх идущей/завершённой финальной реконсиляции.
                if a.stop.load(Ordering::Acquire) {
                    break;
                }
                let prev = a.injected.lock().clone();
                if inject::inject_incremental(&prev, &flat).is_ok() {
                    *a.injected.lock() = flat;
                }
            }
            "auto" => {
                if committed.is_empty() {
                    continue;
                }
                let flat = flatten_breaks(&committed);
                if !live_target_ok(&a.start_fp, &a.abort) {
                    continue;
                }
                let _inj = a.inject_lock.lock();
                // Перепроверка stop под замком — как в always (см. выше).
                if a.stop.load(Ordering::Acquire) {
                    break;
                }
                // already читаем ПОД inject_lock: пока тик ждал замок, финал или
                // стирание черновика могли обновить committed_field — дифф от
                // устаревшего prev поломал бы текст в поле.
                let already = a.committed_field.lock().clone();
                if flat == already {
                    continue;
                }
                if inject::inject_incremental(&already, &flat).is_ok() {
                    *a.committed_field.lock() = flat;
                }
            }
            _ => {}
        }
    }
}

/// Аргументы петли частичных результатов (упакованы, чтобы не плодить параметры).
struct PartialLoopArgs {
    buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    rate: u32,
    app: AppHandle,
    port: u16,
    language: String,
    asr_lock: Arc<Mutex<()>>,
    /// Замок эмиссии клавиш (см. EngineCtx.inject_lock) — общий на движок.
    inject_lock: Arc<Mutex<()>>,
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    injected: Arc<Mutex<String>>,
    committed: Arc<Mutex<String>>,
    start_fp: TargetFingerprint,
    stream_mode: String,
    /// Поколение (seq) диктовки — кладётся в событие "partial" для отбрасывания
    /// устаревших партиалов на фронте.
    seq: u64,
    settings: Settings,
    dict: Vec<postprocess::Dict>,
    snippets: Vec<postprocess::Snippet>,
    corrections: Vec<postprocess::Correction>,
}

/// Фоновая петля живого стриминга: каждые ~700 мс снимает буфер, ресэмплит,
/// гонит через whisper-server БЕЗ гейта и эмитит "partial"; для auto/always
/// дополнительно вставляет текст в поле клавишами.
///
/// Поток НЕ владеет cpal Stream (тот живёт на потоке движка) — через границу
/// потока переходит лишь Arc на буфер сэмплов, поэтому он полностью Send.
fn partial_loop(a: PartialLoopArgs) {
    let min_new16 = 16000 * 3 / 10; // нужно ≥0.3 c нового звука (16к-домен) на тик
    let mut last_len16 = 0usize;
    // Стабилизатор живого префикса: история N=6 партиалов, фиксация по K=2 совпавшим
    // подряд тикам. Монотонный committed → пилюля не переписывает уже показанное
    // начало/середину фразы. Локален для диктовки (новый Start → новая петля → сброс).
    let mut stab = PrefixStabilizer::new(6, 2);
    // P1-1: снимаем только хвост буфера + инкрементальный ресемпл (вместо клона
    // всего буфера и полного ре-ресемпла каждый тик); mono16 только дописывается.
    let mut cursor = 0usize;
    let mut rs = audio::Resampler16k::new(a.rate);
    let mut mono16: Vec<f32> = Vec::new();
    let mut wav_error_logged = false;
    let mut asr_error_logged = false;
    let mut empty_logged = false;
    // P2-4: имя WAV с seq-суффиксом — никакой гонки на общем tmp/partial.wav
    // с петлёй соседней диктовки (cloud и финал суффикс уже имели).
    let wav = paths::tmp_dir().join(format!("partial_{}.wav", a.seq));

    loop {
        std::thread::sleep(Duration::from_millis(500)); // каденс тиков (было 700 — отзывчивее)
        if a.stop.load(Ordering::Acquire) {
            break;
        }

        let (tail, ncur) = audio::tail_since(&a.buffer, cursor);
        cursor = ncur;
        mono16.extend(rs.feed(&tail));
        if mono16.len() < last_len16 + min_new16 {
            continue; // мало нового звука — пропускаем тик
        }
        // last_len16 двигаем НЕ здесь, а после успешного распознавания (ниже): иначе
        // пропущенный тик (занят asr_lock / звук обрезан в тишину) «съедал» бы порог
        // и партиал откладывался до накопления ещё 0.3 c звука.

        // Лёгкая обрезка тишины; слишком короткий звук пропускаем.
        let trimmed = audio::trim_silence(&mono16, 16000);
        if trimmed.len() < 16000 * 3 / 10 {
            continue; // < ~0.3 c полезного звука
        }

        if let Err(e) = audio::write_wav_16k_mono(&wav, &trimmed) {
            if !wav_error_logged {
                dbg_log(&format!("partial: write wav ошибка: {e}"));
                wav_error_logged = true;
            }
            continue;
        }

        // Берём asr-замок неблокирующе: если идёт финал/другая операция — пропускаем тик.
        let txt = {
            let Some(_g) = a.asr_lock.try_lock() else {
                continue;
            };
            if a.stop.load(Ordering::Acquire) {
                break;
            }
            match asr::transcribe_server_partial(a.port, &wav, &a.language) {
                Ok(t) => t,
                Err(e) => {
                    if !asr_error_logged {
                        dbg_log(&format!("partial: whisper-server ошибка: {e:#}"));
                        asr_error_logged = true;
                    }
                    continue; // тик глотает ошибку — частичные результаты best-effort
                }
            }
        };
        // Stop can be raised while the blocking whisper request is in flight.
        // Re-check after it returns so a detached worker cannot publish a live
        // draft after the exact final text has already been inserted/emitted.
        if a.stop.load(Ordering::Acquire) {
            break;
        }

        if txt.trim().is_empty() {
            if !empty_logged {
                dbg_log("partial: whisper-server вернул пустой текст");
                empty_logged = true;
            }
            continue;
        }

        // Снимок успешно распознан — теперь двигаем порог (пропущенные тики звук не «съедают»).
        last_len16 = mono16.len();

        // Стабилизируем префикс: committed (чёрный, монотонный) + volatile (серый хвост).
        let (committed_raw, volatile_raw) = stab.push(&txt);
        let (committed, volatile, full) = clean_live_partial(
            &committed_raw,
            &volatile_raw,
            &a.settings,
            &a.dict,
            &a.snippets,
            &a.corrections,
        );
        if full.is_empty() {
            continue;
        }

        // Пилюля стримит разделённо: text (=committed+volatile) для обратной
        // совместимости, committed/volatile — новый контракт (стабильный + хвост).
        let _ = a.app.emit(
            "partial",
            live_partial_payload(&full, &committed, &volatile, a.seq, None),
        );

        // Живая вставка для auto/always (never — только пилюля).
        if a.abort.load(Ordering::Acquire) {
            continue;
        }
        match a.stream_mode.as_str() {
            // always: поведение НЕ меняем — печатаем сырой партиал (поле = живой текст).
            "always" => live_insert_always(&a, &txt),
            // auto: печатаем КЛАВИШАМИ только стабилизированный committed (тот же
            // источник истины, что и пилюля), а не сырой 2-тиковый префикс.
            "auto" => live_insert_auto_committed(&a, &committed),
            _ => {} // "never": ничего не вставляем
        }
    }
    // Уникальный WAV этой петли больше не нужен (P2-4) — убираем за собой.
    let _ = std::fs::remove_file(&wav);
}

/// Проверка отпечатка окна перед живой вставкой: при смене окна/поля — навсегда
/// (на эту диктовку) выставляем abort и больше не вставляем. Возвращает true,
/// если вставлять МОЖНО (окно то же и abort не выставлен).
fn live_target_ok(start_fp: &TargetFingerprint, abort: &Arc<AtomicBool>) -> bool {
    if abort.load(Ordering::Acquire) {
        return false;
    }
    if !start_fp.is_usable_dictation_target() {
        return false;
    }
    let cur = crate::app_context::detect();
    if !start_fp.matches(&cur) {
        // Окно не наше — СЕЙЧАС не вставляем, но НЕ латчим abort навсегда: если фокус
        // вернётся на исходное поле, продолжим (в чужое окно мы ничего не печатали, поэтому
        // injected/committed всё ещё соответствуют целевому полю). Это чинит «permanent
        // abort» — кратковременная потеря фокуса больше не гасит живую вставку до конца
        // диктовки. Финал-проход независимо перепроверяет окно перед реконсиляцией.
        return false;
    }
    true
}

/// always: на каждый тик сводим напечатанное (`injected`) → `partial` клавишами.
fn live_insert_always(a: &PartialLoopArgs, partial: &str) {
    if !live_target_ok(&a.start_fp, &a.abort) {
        return;
    }
    // Замок эмиссии клавиш: нажатия этой диктовки не чередуются с финалом предыдущей.
    let _inj = a.inject_lock.lock();
    // Гонка осиротевшего тика (P2-5): финал выставляет stop ДО взятия inject_lock —
    // перепроверяем под замком; взведён → тик ничего не печатает.
    if a.stop.load(Ordering::Acquire) {
        return;
    }
    let prev = a.injected.lock().clone();
    if inject::inject_incremental(&prev, partial).is_ok() {
        *a.injected.lock() = partial.to_string();
    }
}

/// auto: печатаем КЛАВИШАМИ только стабилизированный committed-префикс (его считает
/// PrefixStabilizer — тот же источник истины, что и пилюля). Волатильный хвост в поле
/// НЕ печатаем — он может меняться. inject_incremental сам сведёт уже напечатанный
/// committed → новый минимальным backspace (кириллица — 1 backspace на букву).
fn live_insert_auto_committed(a: &PartialLoopArgs, committed: &str) {
    if committed.is_empty() {
        return;
    }
    if !live_target_ok(&a.start_fp, &a.abort) {
        return;
    }
    // Замок эмиссии клавиш (как в always).
    let _inj = a.inject_lock.lock();
    // Перепроверка stop под замком (P2-5) — детачнутый тик не печатает
    // поверх идущего финала (тот выставляет stop ДО взятия inject_lock).
    if a.stop.load(Ordering::Acquire) {
        return;
    }
    // already читаем ПОД замком: финал/стирание черновика могли обновить
    // committed, пока тик ждал, — дифф от устаревшего prev портил бы поле.
    let already = a.committed.lock().clone();
    if committed == already {
        return; // фиксировать нечего нового
    }
    if inject::inject_incremental(&already, committed).is_ok() {
        *a.committed.lock() = committed.to_string();
    }
}

/// Стабилизатор живого префикса: держит историю последних N токен-партиалов и
/// фиксирует (commit) самый длинный общий ведущий токен-префикс, который НЕ менялся
/// K тиков подряд. committed-длина монотонно НЕ убывает (гистерезис) — поэтому
/// пилюля не переписывает уже показанное начало/середину фразы (борьба с тем, что
/// whisper-server на каждом тике заново распознаёт растущий буфер).
///
/// pub(crate): используется и реальной петлёй (engine), и headless `--stream-selftest`
/// (lib.rs), чтобы проверять фиксацию префикса без GUI/микрофона.
pub(crate) struct PrefixStabilizer {
    /// История последних N партиалов (каждый — вектор токенов).
    history: std::collections::VecDeque<Vec<String>>,
    n: usize,
    k: usize,
    committed_len: usize,
    committed: Vec<String>,
}

impl PrefixStabilizer {
    pub(crate) fn new(n: usize, k: usize) -> Self {
        Self {
            history: std::collections::VecDeque::with_capacity(n),
            n: n.max(1),
            k: k.max(1),
            committed_len: 0,
            committed: Vec::new(),
        }
    }

    /// Скормить новый сырой партиал. Возвращает (committed, volatile) как строки:
    /// committed — стабильный префикс (чёрный), volatile — текущий хвост (серый).
    pub(crate) fn push(&mut self, raw: &str) -> (String, String) {
        let toks: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
        if self.history.len() == self.n {
            self.history.pop_front();
        }
        self.history.push_back(toks);

        // Стабильный префикс = общий ведущий токен-префикс последних `depth` партиалов.
        let depth = self.k.min(self.history.len());
        let last = self.history.back().unwrap().clone();
        let mut stable = 0usize;
        'outer: while stable < last.len() {
            let tok = &last[stable];
            for h in self.history.iter().rev().take(depth) {
                if h.get(stable).map(|t| t == tok).unwrap_or(false) {
                    continue;
                }
                break 'outer;
            }
            stable += 1;
        }
        // Фиксируем только накопив ≥K партиалов; committed_len ТОЛЬКО растёт (гистерезис).
        let eligible = self.history.len() >= self.k;
        if eligible && stable > self.committed_len {
            self.committed_len = stable;
            self.committed = last[..stable].to_vec();
        }
        let committed_str = self.committed.join(" ");
        // volatile — хвост ПОСЛЕДНЕГО партиала после committed_len; если он короче
        // committed (whisper укоротил) — хвост пуст, committed держим (не дёргаем экран).
        let volatile_str = if last.len() > self.committed_len {
            last[self.committed_len..].join(" ")
        } else {
            String::new()
        };
        (committed_str, volatile_str)
    }
}

fn stop_and_process(capture: &mut Option<Capture>, ctx: &EngineCtx) {
    let Some(c) = capture.take() else {
        return;
    };
    finish_capture_and_process(c, ctx);
}

fn stop_tap_and_process(capture: &mut Option<Capture>, ctx: &EngineCtx) {
    let Some(c) = capture.take() else {
        return;
    };
    // A sub-180 ms press is the first half of the double-tap gesture, not a
    // useful dictation. Discard it instead of launching VAD/final ASR: that
    // removes model contention and makes the second press truly immediate.
    let _ = c.finish();
    let my_gen = ctx.gen.load(Ordering::SeqCst);
    ctx.recording.store(false, Ordering::SeqCst);
    restore_auto_mute(ctx);
    *ctx.active_target.lock() = None;
    if let Some(mut st) = ctx.partial.lock().take() {
        st.stop.store(true, Ordering::Release);
        st.abort.store(true, Ordering::Release);
        if let Some(join) = st.join.take() {
            finish_partial_without_blocking(join, st.kind);
        }
    }
    if ctx.settings.lock().play_sounds {
        sound::play(false);
    }
    set_status(ctx, "idle");
    clear_cancel_active_if_current(ctx, my_gen);
    dbg_log("hotkey: quick tap discarded as a double-tap gesture candidate");
}

fn finish_capture_and_process(c: Capture, ctx: &EngineCtx) {
    let rate = c.sample_rate();
    // Переполнение буфера записи (audio P2-1): хвост диктовки отброшен —
    // честно предупреждаем (хотя бы раз за запись), текст будет неполным.
    if c.overflowed() {
        dbg_log("stop: буфер записи переполнен (30 мин) — хвост диктовки отброшен");
        emit_error(
            &ctx.app,
            "Запись упёрлась в лимит 30 минут — конец диктовки не записан",
        );
    }
    // finish() дропает cpal Stream и забирает полный буфер.
    let samples = c.finish();
    ctx.recording.store(false, Ordering::SeqCst);
    restore_auto_mute(ctx);
    let stored_target_fp = ctx.active_target.lock().take();
    // Поколение ЭТОЙ диктовки — финал-поток сверит его перед вставкой (C4).
    let my_gen = ctx.gen.load(Ordering::SeqCst);
    // UX: как только пользователь завершил запись, оверлей должен сразу уйти из
    // текстовой плашки в AquaVoice-style spinner, пока мы останавливаем partial-loop
    // и готовим финальный ASR.
    set_status(ctx, "transcribing");

    // Останавливаем петлю частичных результатов, но НИКОГДА не ждём её на
    // единственном engine command thread. Иначе быстрый следующий Start стоит
    // за Stop до 120 мс (local) или 2 с (whisper) и теряет начало речи.
    // Detached final сам сериализуется через asr_lock; устаревшее поколение
    // отсекается до дорогого ASR и перед вставкой.
    let partial_stop_started = Instant::now();
    let pstate = ctx.partial.lock().take();
    if let Some(mut st) = pstate {
        st.stop.store(true, Ordering::Release);
        if let Some(j) = st.join.take() {
            finish_partial_without_blocking(j, st.kind);
        }
        dbg_log(&format!(
            "[lat] stop_wait={}мс",
            partial_stop_started.elapsed().as_millis()
        ));
        // Переносим живое состояние в финальный проход (для inject_incremental реконсиляции).
        let live = LiveState {
            stream_mode: st.stream_mode,
            injected: st.injected,
            committed: st.committed,
            abort: st.abort,
            start_fp: st.start_fp,
        };
        register_pending_live_draft(ctx, my_gen, &live);
        let target_fp = live.start_fp.clone();
        if ctx.settings.lock().play_sounds {
            sound::play(false);
        }
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            if let Err(err) = process_utterance(&ctx2, samples, rate, Some(live), my_gen, target_fp)
            {
                log::error!("process_utterance: {err:#}");
                report_process_err(&ctx2.app, &err);
            }
            // Статус idle эмитим только если новая диктовка ещё не началась —
            // иначе перетёрли бы «recording» свежей диктовки (C3/C4).
            if ctx2.gen.load(Ordering::SeqCst) == my_gen {
                set_status(&ctx2, "idle");
            }
            clear_cancel_active_if_current(&ctx2, my_gen);
        });
        return;
    }

    if ctx.settings.lock().play_sounds {
        sound::play(false);
    }
    // Тяжёлую обработку выносим в отдельный поток, чтобы движок мог принять новую запись.
    let ctx2 = ctx.clone();
    let target_fp = stored_target_fp.unwrap_or_else(|| {
        let actx = crate::app_context::detect();
        let fp = actx.target_fingerprint();
        dbg_log(&format!(
            "stop: target fallback after status {}",
            fp.describe()
        ));
        fp
    });
    std::thread::spawn(move || {
        if let Err(err) = process_utterance(&ctx2, samples, rate, None, my_gen, target_fp) {
            log::error!("process_utterance: {err:#}");
            report_process_err(&ctx2.app, &err);
        }
        if ctx2.gen.load(Ordering::SeqCst) == my_gen {
            set_status(&ctx2, "idle");
        }
        clear_cancel_active_if_current(&ctx2, my_gen);
    });
}

fn restore_auto_mute(ctx: &EngineCtx) {
    restore_auto_mute_arc(&ctx.auto_mute);
}

fn restore_auto_mute_arc(auto_mute: &Arc<Mutex<Option<AutoMuteGuard>>>) {
    if let Some(mut guard) = auto_mute.lock().take() {
        guard.restore();
        dbg_log("auto-mute: system output restored");
    }
}

fn clear_cancel_active_if_current(ctx: &EngineCtx, my_gen: u64) {
    let _activity = ctx.cancel_activity_lock.lock();
    if ctx.gen.load(Ordering::SeqCst) == my_gen
        && !ctx.recording.load(Ordering::SeqCst)
        && !ctx.improve_busy.load(Ordering::SeqCst)
    {
        ctx.cancel_active.store(false, Ordering::SeqCst);
    }
}

fn cancel_current(capture: &mut Option<Capture>, ctx: &EngineCtx) {
    let my_gen = ctx.gen.load(Ordering::SeqCst);
    if let Some(c) = capture.take() {
        let _ = c.finish();
        ctx.recording.store(false, Ordering::SeqCst);
        restore_auto_mute(ctx);
        *ctx.active_target.lock() = None;
        let pstate = ctx.partial.lock().take();
        if let Some(mut st) = pstate {
            st.stop.store(true, Ordering::Release);
            if let Some(j) = st.join.take() {
                let t0 = Instant::now();
                while !j.is_finished() && t0.elapsed() < Duration::from_millis(150) {
                    std::thread::sleep(Duration::from_millis(10));
                }
                if j.is_finished() {
                    let _ = j.join();
                }
            }
            let live = LiveState {
                stream_mode: st.stream_mode,
                injected: st.injected,
                committed: st.committed,
                abort: st.abort,
                start_fp: st.start_fp,
            };
            erase_live_draft(ctx, Some(&live), my_gen);
        }
        {
            let _activity = ctx.cancel_activity_lock.lock();
            ctx.gen.fetch_add(1, Ordering::SeqCst);
            ctx.cancel_active.store(false, Ordering::SeqCst);
        }
        set_status(ctx, "idle");
        dbg_log("cancel: активная диктовка отменена Esc");
        return;
    }
    if !ctx.cancel_active.load(Ordering::SeqCst) && !ctx.improve_busy.load(Ordering::SeqCst) {
        dbg_log("cancel: Esc проигнорирован — активной работы нет");
        return;
    }
    advance_generation_with_live_cleanup(ctx, false);
    *ctx.active_target.lock() = None;
    if ctx.improve_busy.load(Ordering::SeqCst) {
        emit_improve_status(&ctx.app, "cancelled", "Отменено");
    }
    set_status(ctx, "idle");
    dbg_log("cancel: текущее действие инвалидировано Esc");
}

fn improve_selection(ctx: &EngineCtx) {
    if ctx.recording.load(Ordering::SeqCst) {
        emit_improve_status(&ctx.app, "busy", "Сначала завершите диктовку");
        return;
    }
    if ctx.improve_busy.swap(true, Ordering::SeqCst) {
        emit_improve_status(&ctx.app, "busy", "Улучшение уже выполняется");
        return;
    }
    {
        let _activity = ctx.cancel_activity_lock.lock();
        ctx.cancel_active.store(true, Ordering::SeqCst);
    }
    let ctx2 = ctx.clone();
    let my_gen = ctx.gen.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let result = improve_selection_inner(&ctx2, my_gen);
        ctx2.improve_busy.store(false, Ordering::SeqCst);
        clear_cancel_active_if_current(&ctx2, my_gen);
        if let Err(e) = result {
            log::warn!("improve selection: {e:#}");
            emit_improve_status(&ctx2.app, "error", &format!("{e}"));
        }
    });
}

fn improve_selection_inner(ctx: &EngineCtx, my_gen: u64) -> anyhow::Result<()> {
    emit_improve_status(&ctx.app, "copying", "Читаю выделенный текст");
    let selected = match inject::copy_selection_text()? {
        Some(t) => t,
        None => {
            emit_improve_status(
                &ctx.app,
                "no_selection",
                "Выделите текст и нажмите клавишу улучшения",
            );
            return Ok(());
        }
    };
    if ctx.gen.load(Ordering::SeqCst) != my_gen {
        return Ok(());
    }

    emit_improve_status(&ctx.app, "rewriting", "Улучшаю текст");
    let mut s = ctx.settings.lock().clone();
    let cloud_or_remote_rewrite = match s.ai_backend.as_str() {
        "gemini" | "openai_compat" => true,
        "ollama" => !crate::net::is_loopback_base_url(&s.ollama_url),
        _ => false,
    };
    if cloud_or_remote_rewrite {
        s.ai_backend = "off".into();
    }
    let corrections = {
        let conn = ctx.db.lock();
        load_corrections(&conn)
    };
    s.verbatim = false;
    s.remove_fillers = true;
    s.auto_punct = true;
    let mut text = postprocess::process(&selected, &s, &[], &[]);
    text = postprocess::apply_corrections(&text, &corrections);
    text = postprocess::normalize_spaces(&text);
    if text.trim().is_empty() {
        emit_improve_status(
            &ctx.app,
            "no_selection",
            "В выделении нет текста для улучшения",
        );
        return Ok(());
    }

    let actx = crate::app_context::detect();
    let mut tone =
        crate::app_context::category_for(&actx.exe, &actx.title, &s.app_profile_overrides);
    if tone == "neutral" || tone == "verbatim" || tone == "code" {
        tone = "doc".into();
    }
    let (smart_instruction, ai_prompt_context) =
        effective_smart_instruction_for_app(&s, &actx, &tone);
    let context_hint = rewrite_context_hint(ctx, &actx, Some(&text));
    let rewrite_tone = if ai_prompt_context {
        "ai"
    } else {
        tone.as_str()
    };
    let (refined, used_model) = refine_text_with_fallback(
        &s,
        RewriteRequest {
            actx: &actx,
            text: &text,
            tone: rewrite_tone,
            smart_instruction: smart_instruction.as_deref(),
            context_hint: context_hint.as_deref(),
            corrections: &corrections,
            force: true,
        },
    );
    if !used_model {
        let message = if cloud_or_remote_rewrite {
            "Глобальное улучшение не отправляет выделенный текст в облако, применены локальные правила"
        } else {
            "Модель недоступна, применены локальные правила"
        };
        emit_improve_status(&ctx.app, "fallback_rules", message);
    }
    if ctx.gen.load(Ordering::SeqCst) != my_gen {
        return Ok(());
    }

    let cur = crate::app_context::detect();
    let _inj = ctx.inject_lock.lock();
    let _commit = ctx.cancel_activity_lock.lock();
    if ctx.gen.load(Ordering::SeqCst) != my_gen {
        return Ok(());
    }
    let target_fp = actx.target_fingerprint();
    if !target_fp.matches(&cur) {
        emit_improve_status(&ctx.app, "cancelled", "Окно изменилось, вставка отменена");
        return Ok(());
    }
    inject::inject_keep_clipboard(&refined, "clipboard")?;
    emit_improve_status(&ctx.app, "inserted", "Текст улучшен");
    Ok(())
}

fn emit_improve_status(app: &AppHandle, status: &str, message: &str) {
    let _ = app.emit(
        "improve_status",
        serde_json::json!({ "status": status, "message": message }),
    );
}

/// Отправить ошибку финального прохода во фронт: для «нет модели» — специальное
/// предупреждение `no_model`, для прочего — общий `error`.
fn report_process_err(app: &AppHandle, err: &anyhow::Error) {
    if err.downcast_ref::<ModelMissing>().is_some() {
        emit_no_model(app);
    } else {
        emit_error(app, &format!("{err}"));
    }
}

/// Живое состояние диктовки, переданное в финальный проход для реконсиляции.
#[derive(Clone)]
struct LiveState {
    stream_mode: String,
    injected: Arc<Mutex<String>>,
    committed: Arc<Mutex<String>>,
    abort: Arc<AtomicBool>,
    start_fp: TargetFingerprint,
}

struct PendingLiveDraft {
    generation: u64,
    live: LiveState,
}

impl LiveState {
    /// Было ли уже что-то физически напечатано в поле за эту диктовку.
    fn live_inserted(&self) -> bool {
        match self.stream_mode.as_str() {
            "always" => !self.injected.lock().is_empty(),
            "auto" => !self.committed.lock().is_empty(),
            _ => false,
        }
    }
}

/// Финальный ASR не должен слушать длинную тишину: whisper особенно легко
/// галлюцинирует на хвостах и паузах. Для WAV, который уходит в whisper/cloud,
/// оставляем только речевые VAD-острова с небольшим запасом по краям.
fn compact_speech_for_final_asr(
    vad: &Arc<Mutex<Option<crate::vad::SileroVad>>>,
    samples: &[f32],
) -> Vec<f32> {
    const SPEECH_PROB: f32 = 0.35;
    const SIL_SPLIT: usize = 600 * 16;

    if samples.len() < 16000 / 5 {
        return samples.to_vec();
    }

    let chunk = crate::vad::CHUNK;
    let mut speech: Vec<bool> = Vec::with_capacity(samples.len() / chunk + 1);
    {
        let mut vg = vad.lock();
        let Some(v) = vg.as_mut() else {
            return samples.to_vec();
        };
        v.reset();
        for c in samples.chunks(chunk) {
            speech.push(v.process_chunk(c).unwrap_or(1.0) >= SPEECH_PROB);
        }
        v.reset();
    }
    if speech.is_empty() {
        return samples.to_vec();
    }
    if !speech.iter().any(|&x| x) {
        return Vec::new();
    }

    let mut spans: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < speech.len() {
        if !speech[i] {
            i += 1;
            continue;
        }
        let start_chunk = i;
        let mut last_voiced = i;
        let mut j = i + 1;
        while j < speech.len() {
            if speech[j] {
                last_voiced = j;
                j += 1;
                continue;
            }
            let mut k = j;
            while k < speech.len() && !speech[k] {
                k += 1;
            }
            if (k - j) * chunk >= SIL_SPLIT {
                break;
            }
            j = k;
        }
        let start = (start_chunk * chunk).saturating_sub(FINAL_ASR_PAD_SAMPLES);
        let end = (((last_voiced + 1) * chunk) + FINAL_ASR_PAD_SAMPLES).min(samples.len());
        if end > start {
            spans.push((start, end));
        }
        i = j.max(last_voiced + 1);
    }

    if spans.is_empty() {
        return Vec::new();
    }
    let kept: usize = spans.iter().map(|(s, e)| e.saturating_sub(*s)).sum();
    if spans.len() == 1 && kept + 16000 >= samples.len() {
        return samples.to_vec();
    }

    let mut out =
        Vec::with_capacity(kept + FINAL_ASR_JOIN_SILENCE_SAMPLES.saturating_mul(spans.len()));
    let mut previous_end: Option<usize> = None;
    for (idx, (start, end)) in spans.into_iter().enumerate() {
        if idx > 0 {
            let original_gap = previous_end
                .map(|prev| start.saturating_sub(prev))
                .unwrap_or(0);
            let join_silence = compacted_final_asr_join_silence(original_gap);
            out.extend(std::iter::repeat_n(0.0, join_silence));
        }
        out.extend_from_slice(&samples[start..end]);
        previous_end = Some(end);
    }
    out
}

const FINAL_ASR_PAD_SAMPLES: usize = 4800;
const FINAL_ASR_JOIN_SILENCE_SAMPLES: usize = 3200;

fn compacted_final_asr_join_silence(gap_between_padded_spans: usize) -> usize {
    // `spans` already include 300 ms of PAD on both sides. Recover the original
    // speech-to-speech gap for the paragraph decision, then keep an 8 s marker
    // instead of copying arbitrarily long silence into final ASR.
    let paragraph_gap_without_padding =
        PARAGRAPH_GAP_SAMPLES.saturating_sub(FINAL_ASR_PAD_SAMPLES * 2);
    if gap_between_padded_spans >= paragraph_gap_without_padding {
        PARAGRAPH_GAP_SAMPLES
    } else {
        FINAL_ASR_JOIN_SILENCE_SAMPLES
    }
}

fn final_local_asr_samples<'a>(_trimmed: &'a [f32], speech_compacted: &'a [f32]) -> &'a [f32] {
    // Keep resident engines on the exact same pause-compacted audio that is
    // written to the replayable WAV. Feeding GigaAM the pre-compaction buffer
    // made its VAD split one spoken word into independent hypotheses; the
    // segment renderer then inserted a real space between those fragments.
    speech_compacted
}

fn generation_is_current(current: u64, expected: u64) -> bool {
    current == expected
}

fn final_generation_is_current(ctx: &EngineCtx, expected: u64) -> bool {
    generation_is_current(ctx.gen.load(Ordering::Acquire), expected)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FinalPipelineDecision {
    Continue,
    Reject { erase_live_draft: bool },
}

/// Decide whether a text hypothesis may enter the commit pipeline. History is
/// written only after that pipeline successfully inserts the final text, so a
/// `Reject` branch is also an explicit "no history" decision.
fn decide_final_pipeline(accepted_text: bool, live_inserted: bool) -> FinalPipelineDecision {
    if accepted_text {
        FinalPipelineDecision::Continue
    } else {
        FinalPipelineDecision::Reject {
            erase_live_draft: live_inserted,
        }
    }
}

fn final_text_is_insertable(text: &str) -> bool {
    !text.trim().is_empty() || text.contains('\n')
}

/// Стереть уже напечатанный живой черновик (режимы always/auto): голосовая
/// «отмена» или команды, съевшие весь текст, не должны оставлять в поле
/// черновик, набранный петлёй партиалов (включая само слово «отмена»).
/// Семантика «отмены»: в поле НИЧЕГО не остаётся.
///
/// Повторяет проверки финальной реконсиляции: gen-guard под inject_lock
/// (новая диктовка уже могла печатать — чужое не трогаем), abort и отпечаток
/// окна (чужое поле не трогаем). prev перечитывается ПОД замком (детачнутый
/// тик мог дописать черновик, пока мы ждали), стирание — prev → "" через
/// inject_incremental (минимальное число Backspace), после чего prev в Arc
/// обнуляется, чтобы повторный проход ничего не стирал дважды.
fn erase_live_draft(ctx: &EngineCtx, live: Option<&LiveState>, my_gen: u64) {
    let Some(l) = live else { return };
    if !l.live_inserted() {
        return; // живой вставки не было — стирать нечего
    }
    let cur = crate::app_context::detect();
    let _inj = ctx.inject_lock.lock();
    let _commit = ctx.cancel_activity_lock.lock();
    let _ = erase_live_draft_locked(ctx, l, my_gen, &cur);
}

/// Same operation with keyboard + generation locks already held. Keeping the
/// generation check and key emission in one critical section lets Start/Esc
/// safely take ownership of an older detached draft before bumping `gen`.
fn erase_live_draft_locked(
    ctx: &EngineCtx,
    l: &LiveState,
    expected_current_gen: u64,
    cur: &crate::app_context::AppContext,
) -> bool {
    if ctx.gen.load(Ordering::SeqCst) != expected_current_gen {
        dbg_log("отмена: поколение устарело — черновик не трогаем");
        return false;
    }
    if l.abort.load(Ordering::Acquire) {
        dbg_log("отмена: живая вставка была прервана — черновик не трогаем");
        return false;
    }
    if !l.start_fp.matches(cur) {
        dbg_log("отмена: окно сменилось — чужое поле не трогаем");
        return false;
    }
    // prev = что физически в поле (injected для always, committed для auto).
    let prev_arc = if l.stream_mode == "always" {
        &l.injected
    } else {
        &l.committed
    };
    let prev = prev_arc.lock().clone();
    if prev.is_empty() {
        return true;
    }
    match inject::inject_incremental(&prev, "") {
        Ok(()) => {
            *prev_arc.lock() = String::new();
            true
        }
        Err(e) => {
            log::warn!("отмена: стирание черновика не удалось: {e}");
            false
        }
    }
}

fn register_pending_live_draft(ctx: &EngineCtx, generation: u64, live: &LiveState) {
    // Register the ownership token even when the ledger is still empty: a
    // partial worker may already be inside inject_lock and update the shared
    // Arc immediately after Stop. Start/Esc inspect the ledger only after
    // acquiring that same lock, so a genuinely empty token never triggers a
    // slow target lookup.
    if matches!(live.stream_mode.as_str(), "always" | "auto") {
        *ctx.pending_live_draft.lock() = Some(PendingLiveDraft {
            generation,
            live: live.clone(),
        });
    }
}

fn take_pending_live_draft_from_slot(
    slot: &mut Option<PendingLiveDraft>,
    generation: u64,
) -> Option<PendingLiveDraft> {
    if slot
        .as_ref()
        .is_some_and(|pending| pending.generation == generation)
    {
        slot.take()
    } else {
        None
    }
}

fn take_pending_live_draft(ctx: &EngineCtx, generation: u64) -> Option<PendingLiveDraft> {
    take_pending_live_draft_from_slot(&mut ctx.pending_live_draft.lock(), generation)
}

fn pending_live_draft_is_registered(ctx: &EngineCtx, generation: u64) -> bool {
    ctx.pending_live_draft
        .lock()
        .as_ref()
        .is_some_and(|pending| pending.generation == generation)
}

/// Claim and erase a detached draft as the current generation's terminal
/// action. The slot claim happens while both commit locks are held, so a new
/// Start/Esc cannot bump the generation between ownership transfer and erase.
fn erase_registered_live_draft(ctx: &EngineCtx, generation: u64) {
    if !pending_live_draft_is_registered(ctx, generation) {
        return;
    }
    let cur = crate::app_context::detect();
    let _inj = ctx.inject_lock.lock();
    let _commit = ctx.cancel_activity_lock.lock();
    if let Some(pending) = take_pending_live_draft(ctx, generation) {
        let _ = erase_live_draft_locked(ctx, &pending.live, generation, &cur);
    }
}

/// Fast phase for a new Start. Claim the old slot and bump generation while
/// holding the same locks as final commit, but never query Accessibility here.
/// The returned draft is cleaned only after CoreAudio is already capturing.
fn advance_generation_for_start(ctx: &EngineCtx) -> (u64, Option<PendingLiveDraft>) {
    let _inj = ctx.inject_lock.lock();
    let _activity = ctx.cancel_activity_lock.lock();
    let current = ctx.gen.load(Ordering::SeqCst);
    let inherited = take_pending_live_draft(ctx, current);
    let next = ctx.gen.fetch_add(1, Ordering::SeqCst) + 1;
    ctx.cancel_active.store(true, Ordering::SeqCst);
    (next, inherited)
}

/// Erase a slot already claimed by a newer generation. The token was removed
/// before that generation bump, and both key emission and the generation check
/// are serialized here, so the stale final cannot race this cleanup and the new
/// partial loop has not started yet.
fn erase_claimed_live_draft_locked_to_generation(
    ctx: &EngineCtx,
    pending: Option<&PendingLiveDraft>,
    current_generation: u64,
    current_context: &crate::app_context::AppContext,
) -> bool {
    let Some(pending) = pending else {
        return false;
    };
    if !pending.live.live_inserted() {
        return false;
    }
    let _inj = ctx.inject_lock.lock();
    let _activity = ctx.cancel_activity_lock.lock();
    erase_live_draft_locked(ctx, &pending.live, current_generation, current_context)
}

fn erase_claimed_live_draft_with_fresh_context(
    ctx: &EngineCtx,
    pending: Option<&PendingLiveDraft>,
    current_generation: u64,
) -> bool {
    let Some(pending) = pending else {
        return false;
    };
    if !pending.live.live_inserted() {
        return false;
    }
    let current_context = detect_current_context();
    erase_claimed_live_draft_locked_to_generation(
        ctx,
        Some(pending),
        current_generation,
        &current_context,
    )
}

/// Invalidate an older detached final without orphaning its physical partial.
/// Whichever side acquires the commit locks first owns the old draft: the final
/// may commit it, or Start/Esc erases it and advances the generation.
fn advance_generation_with_live_cleanup(ctx: &EngineCtx, cancel_active: bool) -> u64 {
    // Esc may afford a synchronous target check; keep inject_lock held so an
    // in-flight partial finishes its ledger update before we decide whether a
    // physical draft exists, and a final cannot commit during detection.
    let _inj = ctx.inject_lock.lock();
    let current = ctx.gen.load(Ordering::SeqCst);
    let detected = ctx
        .pending_live_draft
        .lock()
        .as_ref()
        .filter(|pending| pending.generation == current)
        .filter(|pending| pending.live.live_inserted())
        .map(|_| detect_current_context());
    let _activity = ctx.cancel_activity_lock.lock();
    if let Some(pending) = take_pending_live_draft(ctx, current) {
        if let Some(cur) = detected.as_ref() {
            let _ = erase_live_draft_locked(ctx, &pending.live, current, cur);
        }
    }
    let next = ctx.gen.fetch_add(1, Ordering::SeqCst) + 1;
    ctx.cancel_active.store(cancel_active, Ordering::SeqCst);
    next
}

/// Error exits before final reconciliation must not leave a speculative live
/// partial in the target field. The actual erasure remains guarded by target,
/// generation, abort, and the exact text tracked in `LiveState`.
fn erase_live_draft_on_error<T>(
    ctx: &EngineCtx,
    my_gen: u64,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    if result.is_err() {
        erase_registered_live_draft(ctx, my_gen);
    }
    result
}

fn process_utterance(
    ctx: &EngineCtx,
    samples: Vec<f32>,
    rate: u32,
    live: Option<LiveState>,
    my_gen: u64,
    mut target_fp: TargetFingerprint,
) -> anyhow::Result<()> {
    // Live auto/always may type partials before the final target check. Preserve
    // the original pre-dictation tail now; a later Accessibility snapshot would
    // already contain VoxFlow's own partial and would be an invalid anchor for
    // manual-correction learning.
    let field_before_context = target_fp.captured_context().field_text.clone();
    let field_before_dictation = compact_learning_text(&field_before_context);
    if !final_generation_is_current(ctx, my_gen) {
        dbg_log("финал: поколение устарело до препроцессинга — ASR пропущен");
        return Ok(());
    }
    if samples.is_empty() {
        // Первый tap double-tap жеста может завершиться до первого CoreAudio
        // callback. Это управляющий жест, поэтому не мигаем ложным norecog.
        dbg_log("финал: пустой короткий tap — нечего распознавать, norecog подавлен");
        // Normally no partial can exist without captured samples. Keep the
        // invariant defensive in case capture/live callbacks race at shutdown.
        erase_registered_live_draft(ctx, my_gen);
        return Ok(());
    }
    let s = ctx.settings.lock().clone();
    // Что-то уже физически напечатано клавишами (always/auto) за эту диктовку.
    let mut live_inserted = live.as_ref().map(|l| l.live_inserted()).unwrap_or(false);

    let t_all = Instant::now();
    let mut context_ms = 0u64;
    let t_pre = Instant::now();
    let mono16 = audio::resample_to_16k(&samples, rate);
    let trimmed = audio::trim_silence(&mono16, 16000);
    let speech_trimmed = compact_speech_for_final_asr(&ctx.vad_final, &trimmed);
    let pre_ms = t_pre.elapsed().as_millis() as u64;
    let capture_ms = samples.len().saturating_mul(1000) as u64 / rate.max(1) as u64;
    dbg_log(&format!(
        "финал: audio raw={}мс mono={}мс trimmed={}мс speech={}мс",
        capture_ms,
        mono16.len().saturating_mul(1000) as u64 / 16000,
        trimmed.len().saturating_mul(1000) as u64 / 16000,
        speech_trimmed.len().saturating_mul(1000) as u64 / 16000
    ));
    if speech_trimmed.len() < 16000 / 5 {
        // < ~0.2 c полезного звука — считаем, что речи не было
        dbg_log("финал: VAD не нашёл речь — распознавание пропущено");
        // A speculative live partial is not stronger evidence than the final
        // VAD decision. Remove only VoxFlow's known draft under the existing
        // target + generation guards.
        erase_registered_live_draft(ctx, my_gen);
        if should_emit_norecog(capture_ms) {
            let _ = ctx.app.emit(
                "norecog",
                serde_json::json!({ "message": "Не услышал речь — проверьте микрофон" }),
            );
        } else {
            dbg_log("финал: короткий управляющий tap — norecog подавлен");
        }
        return Ok(());
    }

    // Уникальное имя WAV на диктовку (C4): исключает гонку на общем файле, когда
    // финал предыдущей диктовки ещё в полёте, а уже стартовала следующая.
    let wav = paths::unique_tmp_path(&format!("utterance_{my_gen}"), "wav");
    let _wav_guard = paths::TempFileGuard::new(wav.clone());
    erase_live_draft_on_error(
        ctx,
        my_gen,
        audio::write_wav_16k_mono(&wav, &speech_trimmed),
    )?;
    let local_samples = final_local_asr_samples(&trimmed, &speech_trimmed);

    // Словарь, сниппеты и выученные исправления из БД (под локом).
    let (dict, snippets, corrections) = {
        let conn = ctx.db.lock();
        (
            load_dict(&conn),
            load_snippets(&conn),
            load_corrections(&conn),
        )
    };

    // ── ASR: приоритет облачного STT-провайдера, иначе Gemini, иначе локальный ASR-пайплайн ──
    // Финальный whisper-проход сериализуем тем же asr_lock, что и тики partial,
    // чтобы он никогда не пересёкся с частичным запросом (петля к этому моменту
    // уже остановлена и приджойнена — это пояс поверх подтяжек).
    //
    // ВАЖНО: облачный текст НЕ проходит whisper-гейт уверенности (тот завязан на
    // verbose_json локального whisper). Пустой ответ облака трактуем как norecog
    // ниже — общим путём (как и при отклонении гейта локальным проходом).
    // Avalon — основной STT по умолчанию, НО без ключа НЕ делаем бессмысленных сетевых
    // попыток (лишний RTT/таймаут): мгновенно работаем локально. «Умный
    // фолбэк» из решения пользователя: облако активно, только когда ключ реально есть
    // (общий хелпер cloud_stt_active — та же проверка, что в гарде старта и петле).
    let use_cloud_stt = s.cloud_stt_active();
    let use_cloud_gemini =
        s.cloud_asr && s.ai_backend == "gemini" && crate::gemini::available(&s.ai_api_key);
    // Local recognition can reuse the full target snapshot captured before the
    // overlay appeared. A fresh query is still mandatory before any remote ASR
    // or potentially remote rewrite, and always again under inject_lock.
    let needs_network_context_guard = use_cloud_stt || use_cloud_gemini;
    let asr_target = if target_fp.is_usable_dictation_target() && !needs_network_context_guard {
        dbg_log("финал: локальный ASR использует контекст, снятый при нажатии");
        Some(target_fp.captured_context())
    } else {
        let context_started = Instant::now();
        let detected = current_or_restored_target(ctx, &mut target_fp, "до ASR");
        context_ms += context_started.elapsed().as_millis() as u64;
        detected
    };
    let Some(asr_actx) = asr_target else {
        erase_registered_live_draft(ctx, my_gen);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    };
    let asr_tone =
        crate::app_context::category_for(&asr_actx.exe, &asr_actx.title, &s.app_profile_overrides);
    let asr_prompt = if use_cloud_stt {
        let asr_previous_context_tail = asr_actx
            .focused_text_tail(ASR_PROMPT_PREVIOUS_CHARS)
            .or_else(|| last_dictation_context(ctx, &asr_actx));
        build_asr_prompt(
            &asr_actx,
            &asr_tone,
            asr_previous_context_tail.as_deref(),
            &dict,
            &snippets,
            &corrections,
        )
    } else {
        None
    };
    if !final_generation_is_current(ctx, my_gen) {
        dbg_log("финал: поколение устарело перед ASR — дорогой вызов пропущен");
        return Ok(());
    }
    let t0 = Instant::now();
    // Язык диктовки для бейджа overlay: Some("ru"/"en") у локальных резидентных
    // маршрутов, None (бейдж скрыт) у облака/whisper.
    let mut lang_badge: Option<&'static str> = None;
    let raw = if use_cloud_stt {
        // Облачный провайдер — основной путь. Сетевой вызов БЕЗ asr_lock
        // (asr_lock сериализует только whisper-server; облако к нему не обращается).
        match crate::cloud_stt::transcribe_with_prompt(&s, &wav, asr_prompt.as_deref()) {
            Ok(t) if !t.trim().is_empty() => {
                emit_stt_mode(&ctx.app, &s.stt_provider, false);
                // Облако без гейта уверенности — дешёвый анти-повтор обязателен.
                postprocess::dedup_repeated_ngrams(&t)
            }
            res => {
                // Ошибка ИЛИ пустой ответ → решаем по флагу fallback.
                match res {
                    Err(e) => log::warn!("облачный STT ({}) ошибка: {e}", s.stt_provider),
                    Ok(_) => log::warn!("облачный STT ({}) вернул пусто", s.stt_provider),
                }
                if s.stt_fallback_local {
                    if !final_generation_is_current(ctx, my_gen) {
                        dbg_log("финал: облачный запрос устарел — local fallback не запускаем");
                        return Ok(());
                    }
                    log::warn!("облачный STT недоступен — откат на локальное распознавание");
                    emit_stt_mode(&ctx.app, "local", true);
                    let (text, lang) = erase_live_draft_on_error(
                        ctx,
                        my_gen,
                        local_asr(ctx, &s, &dict, &snippets, &wav, local_samples, my_gen),
                    )?;
                    lang_badge = lang;
                    text
                } else {
                    // Fallback выключен — честно сообщаем об ошибке и выходим.
                    erase_registered_live_draft(ctx, my_gen);
                    if s.play_sounds {
                        sound::fail();
                    }
                    emit_error(&ctx.app, "Облачный STT недоступен");
                    let _ = std::fs::remove_file(&wav);
                    return Ok(());
                }
            }
        }
    } else if use_cloud_gemini {
        match crate::gemini::transcribe(&s.ai_api_key, &s.ai_model, &wav, &s.language) {
            Ok(t) => postprocess::dedup_repeated_ngrams(&t),
            Err(e) => {
                if !final_generation_is_current(ctx, my_gen) {
                    dbg_log("финал: Gemini ASR устарел — local fallback не запускаем");
                    return Ok(());
                }
                log::warn!("облачный ASR (Gemini) ошибка: {e}; откат на локальное распознавание");
                let (text, lang) = erase_live_draft_on_error(
                    ctx,
                    my_gen,
                    local_asr(ctx, &s, &dict, &snippets, &wav, local_samples, my_gen),
                )?;
                lang_badge = lang;
                text
            }
        }
    } else {
        let (t, lang) = erase_live_draft_on_error(
            ctx,
            my_gen,
            local_asr(ctx, &s, &dict, &snippets, &wav, local_samples, my_gen),
        )?;
        lang_badge = lang;
        t
    };
    let ms = t0.elapsed().as_millis() as u64;
    if !final_generation_is_current(ctx, my_gen) {
        dbg_log("финал: ASR завершился для устаревшего поколения — постобработку пропускаем");
        return Ok(());
    }
    // Бейдж языка в пилюле: статус-объект { status, lang } по контракту overlay
    // (legacy-строки "idle"/"recording"/"transcribing" остаются как были).
    // Только если эта диктовка ещё актуальна — не перетираем статус следующей.
    if let Some(l) = lang_badge {
        if ctx.gen.load(Ordering::SeqCst) == my_gen {
            let _ = ctx.app.emit(
                "status",
                serde_json::json!({ "status": "transcribing", "lang": l, "seq": my_gen }),
            );
        }
    }
    let raw = postprocess::dedup_repeated_ngrams(&raw);

    if let FinalPipelineDecision::Reject { .. } =
        decide_final_pipeline(!raw.trim().is_empty(), live_inserted)
    {
        dbg_log(&format!(
            "финал: ASR вернул пустой текст за {ms}мс (pre={pre_ms}мс)"
        ));
        // Гейт уверенности/VAD отклонил (невнятно / тишина / чужой язык).
        // A live partial is speculative and does not carry the final word
        // confidence gate, so keeping it here would turn rejected noise into a
        // permanent phantom insertion. Erase only VoxFlow's exact known draft;
        // `erase_live_draft` refuses to touch another field or generation.
        //
        // ВАЖНО: это не системная ошибка. Пользователь мог случайно тапнуть хоткей,
        // отпустить слишком рано или говорить тише VAD-порога. Раньше здесь играл
        // sound::fail(), из-за чего нормальные "ничего не распознано" ощущались как
        // поломка приложения. Реальные ошибки (нет модели, облако недоступно и т.п.)
        // по-прежнему идут через emit_error/no_model и отдельные fail-пути.
        dbg_log("финал отклонён — безопасно убираем живой черновик");
        erase_registered_live_draft(ctx, my_gen);
        let _ = ctx.app.emit(
            "norecog",
            serde_json::json!({ "message": "Не расслышал — повторите чётче" }),
        );
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // Контекст окна нужен и для тона, и для payload Ollama, и для rule-based
    // продолжения фразы. Детектим один раз после ASR и до постобработки.
    // The target was already validated immediately before ASR and is validated
    // again under inject_lock immediately before insertion. Re-running the same
    // synchronous macOS AppleScript here added latency without strengthening the
    // final TOCTOU guard, so reuse the captured context for tone/postprocessing.
    let mut actx = asr_actx.clone();
    dbg_log(&format!(
        "app: exe={} title_len={} → {}",
        actx.exe,
        actx.title.chars().count(),
        actx.category
    ));
    // Постобработка (правила) + выученные исправления.
    let t_post = Instant::now();
    let exact_snippet = postprocess::expand_matching_snippet(&raw, &snippets);
    let snippet_expanded = exact_snippet.is_some();
    let mut text =
        exact_snippet.unwrap_or_else(|| postprocess::process(&raw, &s, &dict, &snippets));
    if !snippet_expanded {
        text = postprocess::apply_corrections(&text, &corrections);
    }
    let post_ms = t_post.elapsed().as_millis() as u64;
    if let FinalPipelineDecision::Reject { .. } =
        decide_final_pipeline(final_text_is_insertable(&text), live_inserted)
    {
        // Постобработка отвергла финал: speculative live draft тоже не должен
        // оставаться в поле.
        erase_registered_live_draft(ctx, my_gen);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // Голосовые команды оставляем как совместимость, но снимаем их ДО LLM:
    // форматирование должно быть автоматическим, а модель не должна съедать хвостовое
    // "отмена"/"абзац" до того, как движок успел распознать команду.
    if !snippet_expanded {
        text = postprocess::normalize_spaces(&text);
        text = match crate::voice_cmds::apply_voice_commands(&text) {
            crate::voice_cmds::CmdOutcome::Cancel => {
                dbg_log("финал: голосовая команда «отмена» — вставка и история пропущены");
                erase_registered_live_draft(ctx, my_gen);
                let _ = std::fs::remove_file(&wav);
                return Ok(());
            }
            crate::voice_cmds::CmdOutcome::Text(t) => t,
        };
    }
    if let FinalPipelineDecision::Reject { .. } =
        decide_final_pipeline(!text.is_empty(), live_inserted)
    {
        erase_registered_live_draft(ctx, my_gen);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // A local ASR pass can take seconds. If a configured rewrite backend may
    // send text off-device, validate the target again immediately before that
    // possible egress; the earlier cloud-ASR guard is too old by this point.
    if !snippet_expanded && potentially_remote_rewrite(&s) {
        if ctx.gen.load(Ordering::SeqCst) != my_gen {
            dbg_log("финал: поколение устарело до remote rewrite — данные не отправляем");
            let _ = std::fs::remove_file(&wav);
            return Ok(());
        }
        let context_started = Instant::now();
        let fresh_target =
            current_or_restored_target(ctx, &mut target_fp, "перед удалённым rewrite");
        context_ms += context_started.elapsed().as_millis() as u64;
        let Some(fresh_actx) = fresh_target else {
            erase_registered_live_draft(ctx, my_gen);
            let _ = std::fs::remove_file(&wav);
            return Ok(());
        };
        if ctx.gen.load(Ordering::SeqCst) != my_gen {
            dbg_log("финал: поколение устарело во время target-check перед rewrite");
            let _ = std::fs::remove_file(&wav);
            return Ok(());
        }
        actx = fresh_actx;
    }

    // Тон по приложению считаем через category_for — он учитывает пользовательские
    // app_profile_overrides (ветка B) ПЕРЕД встроенной таблицей классификации.
    let tone = crate::app_context::category_for(&actx.exe, &actx.title, &s.app_profile_overrides);
    // Prefer what is already in the focused field over VoxFlow's own memory.
    // This keeps sentence casing/punctuation continuous even when the existing
    // text was typed manually or inserted by another application.
    let visible_previous =
        visible_previous_context_tail(&actx, &field_before_context, live_inserted);
    let previous_context_tail = previous_context_with_memory_boundary(ctx, &actx, visible_previous);

    // ── «Умный» рерайт под стиль активного приложения (Gemini/Ollama/OpenAI-compatible) ──
    // verbatim/neutral и встроенный AI-профиль LLM не зовут: вставка должна быть
    // мгновенной. Явный smart prompt / правило для конкретной нейросети остаётся
    // opt-in и может синхронно отрефайнить текст.
    let explicit_smart_instruction =
        ai_prompt_rule_for_app(&s, &actx).is_some() || effective_smart_instruction(&s).is_some();
    let (smart_instruction, ai_prompt_context) =
        effective_smart_instruction_for_app(&s, &actx, &tone);
    let context_hint = rewrite_context_hint(ctx, &actx, None);
    let rewrite_tone = if ai_prompt_context {
        "ai"
    } else {
        tone.as_str()
    };
    let smart_active = smart_instruction.is_some();
    let llm_eligible = !snippet_expanded
        && final_rewrite_eligible(&s, rewrite_tone, smart_active, explicit_smart_instruction);
    let t_llm = Instant::now();
    if llm_eligible {
        text = refine_text_with_fallback(
            &s,
            RewriteRequest {
                actx: &actx,
                text: &text,
                tone: rewrite_tone,
                smart_instruction: smart_instruction.as_deref(),
                context_hint: context_hint.as_deref(),
                corrections: &corrections,
                force: false,
            },
        )
        .0;
    }
    let llm_ms = t_llm.elapsed().as_millis() as u64;

    // C5: после apply_corrections и LLM-рерайта пробелы могли «съехать»
    // (replace_ci — сырая подстрочная замена; LLM иногда добавляет лишние пробелы).
    // normalize_spaces внутри postprocess::process отрабатывает РАНЬШЕ этих шагов,
    // поэтому нормализуем ещё раз — финально, перед вставкой.
    if !snippet_expanded {
        text = postprocess::normalize_spaces(&text);
        if conversational_continuation_enabled(&tone) {
            text = postprocess::soften_false_sentence_breaks(&text);
        }
        text = continue_from_previous_context(&text, previous_context_tail.as_deref(), &tone);
        text = postprocess::normalize_spaces(&text);
    }
    if should_preserve_unfinished_ending(&s, &tone, snippet_expanded) {
        text = postprocess::preserve_unfinished_ending(&text, &raw);
    }
    if let FinalPipelineDecision::Reject { .. } =
        decide_final_pipeline(final_text_is_insertable(&text), live_inserted)
    {
        erase_registered_live_draft(ctx, my_gen);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // ── Generation-guard (C4) ──
    // Если за время обработки уже стартовала НОВАЯ диктовка — этот поток «осиротел».
    // Не вставляем ничего (иначе многократная вставка при быстрой диктовке подряд).
    // Чистим за собой временный WAV и выходим.
    if ctx.gen.load(Ordering::SeqCst) != my_gen {
        dbg_log("финал: поколение устарело (началась новая диктовка) — вставку пропускаем");
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // ── Финальная вставка ──
    // always/auto (и live-текст уже физически вставлен): реконсиляция
    // напечатанного → финальный текст
    //   КЛАВИШАМИ (inject_incremental) — предыдущее тоже печаталось, диффы валидны.
    //   При смене окна (abort) чужое поле не трогаем.
    // never / без петли: обычная вставка целиком (clipboard/type как раньше).
    let live_mode = live
        .as_ref()
        .map(|l| l.stream_mode.as_str())
        .unwrap_or("never");
    // Target detection may call platform accessibility APIs. Keep it outside
    // the keyboard lock so a new hotkey can invalidate this generation while
    // detection is running instead of waiting behind it.
    let context_started = Instant::now();
    let final_target = current_or_restored_target(ctx, &mut target_fp, "перед вставкой");
    context_ms += context_started.elapsed().as_millis() as u64;
    let Some(final_target_actx) = final_target else {
        // Do not press keys without a verified target, but never lose a fully
        // recognized result: leave exactly this text ready for manual paste.
        erase_registered_live_draft(ctx, my_gen);
        if let Err(error) = inject::set_clipboard_text(&text) {
            dbg_log(&format!(
                "финал: не удалось сохранить текст в clipboard: {error:#}"
            ));
        } else {
            emit_error(
                &ctx.app,
                "Активное окно не удалось подтвердить. Текст сохранён в буфере обмена",
            );
        }
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    };
    // Commit protocol: inject_lock serializes key emission; cancel_activity_lock
    // makes generation-check + paste atomic with Start's generation bump. The
    // slow target query above is deliberately outside both locks.
    let inject_guard = ctx.inject_lock.lock();
    let commit_guard = ctx.cancel_activity_lock.lock();
    if ctx.gen.load(Ordering::SeqCst) != my_gen {
        dbg_log("финал: поколение устарело после target-check — вставку пропускаем");
        drop(inject_guard);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    if ctx.last_injected_gen.load(Ordering::SeqCst) == my_gen {
        dbg_log("финал: это поколение уже вставлено — пропускаем");
        drop(inject_guard);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    // A partial worker that was already inside inject_lock at Stop may have
    // finished after the earlier snapshot. Re-read the shared ledger under the
    // commit lock, then require the still-registered ownership token before
    // touching a physical live draft. Start/Esc remove that token before they
    // invalidate this generation, so a stale final can never rewrite their text.
    live_inserted = live.as_ref().map(|l| l.live_inserted()).unwrap_or(false);
    if live_inserted && !pending_live_draft_is_registered(ctx, my_gen) {
        dbg_log("финал: ownership live-черновика уже передан Start/Esc — вставку пропускаем");
        drop(commit_guard);
        drop(inject_guard);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    let mut final_inserted = false;
    let mut insert_error: Option<String> = None;
    let t_inj = Instant::now();
    match (live.as_ref(), live_mode) {
        (Some(l), "always") | (Some(l), "auto") if live_inserted => {
            if l.abort.load(Ordering::Acquire) {
                dbg_log("финал: окно сменилось — реконсиляцию пропускаем");
            } else if !target_fp.matches(&final_target_actx) {
                l.abort.store(true, Ordering::Release);
                dbg_log("финал: целевое окно изменилось — реконсиляцию пропускаем");
            } else {
                // prev = что уже физически в поле (injected для always, committed для auto).
                let live_ledger = if l.stream_mode == "always" {
                    Arc::clone(&l.injected)
                } else {
                    Arc::clone(&l.committed)
                };
                let prev = live_ledger.lock().clone();
                if text.contains('\n') {
                    // Абзацы должны быть автоматическими, но Enter в чатах опасен.
                    // Поэтому живой черновик стираем клавишами, а финальный
                    // многострочный текст вставляем одним безопасным clipboard-paste.
                    let _ = inject::set_clipboard_text(&text)
                        .map_err(|e| log::warn!("clipboard final text: {e}"));
                    match inject::inject_incremental(&prev, "") {
                        Err(e) => {
                            log::warn!("финальная очистка live-черновика: {e}");
                            insert_error = Some(format!("{e:#}"));
                        }
                        Ok(()) => {
                            // The physical draft is already gone. Update the
                            // ledger before the separate paste so a failed paste
                            // can never make the next Start backspace it twice.
                            *live_ledger.lock() = String::new();
                            if let Err(e) = inject::inject_keep_clipboard(&text, "clipboard") {
                                log::warn!("финальная clipboard-вставка с абзацами: {e}");
                                insert_error = Some(format!("{e:#}"));
                            } else {
                                *live_ledger.lock() = text.clone();
                                final_inserted = true;
                            }
                        }
                    }
                } else {
                    let flat = flatten_breaks(&text);
                    let _ = inject::set_clipboard_text(&flat)
                        .map_err(|e| log::warn!("clipboard final text: {e}"));
                    if let Err(e) = inject::inject_incremental(&prev, &flat) {
                        log::warn!("финальная реконсиляция: {e}");
                        insert_error = Some(format!("{e:#}"));
                    } else {
                        *live_ledger.lock() = flat;
                        final_inserted = true;
                    }
                }
            }
        }
        _ => {
            // never-режим или петли не было — поведение как раньше (вставка целиком).
            // Ошибку пробрасываем ПОСЛЕ уборки временного WAV (иначе утечка в tmp).
            if let Err(e) = inject::inject_keep_clipboard(&text, &s.paste_method) {
                dbg_log(&format!("финал: ошибка вставки: {e:#}"));
                drop(inject_guard);
                let _ = std::fs::remove_file(&wav);
                return Err(e);
            }
            final_inserted = true;
        }
    }
    if final_inserted {
        // Clear the ownership token only after the field contains the confirmed
        // final text. Until this exact point a concurrent Start/Esc is blocked
        // by the two guards and may not mistake the final for a stale draft.
        let _ = take_pending_live_draft(ctx, my_gen);
        ctx.last_injected_gen.store(my_gen, Ordering::SeqCst);
    }
    drop(inject_guard); // освобождаем замок клавиш сразу после вставки
    drop(commit_guard); // новый Start блокируется только на самом paste
    if !final_inserted {
        if let Some(err) = insert_error {
            dbg_log(&format!(
                "финал: вставка не выполнена — история/transcript пропущены: {err}"
            ));
            emit_error(&ctx.app, insertion_failure_help());
        } else {
            dbg_log("финал: вставка не выполнена — история/transcript пропущены");
        }
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    if final_inserted {
        remember_dictation_context(
            ctx,
            &actx,
            &text,
            postprocess::utterance_has_explicit_terminal_punctuation(&raw),
        );
        emit_final_preview(&ctx.app, &text, my_gen, lang_badge);
    }
    let inject_ms = t_inj.elapsed().as_millis() as u64;
    // Сквозной замер этапов финала: отпускание клавиши → текст в поле.
    dbg_log(&format!(
        "[lat] gen={my_gen} pre={pre_ms}мс context={context_ms}мс asr={ms}мс post={post_ms}мс llm={llm_ms}мс inject={inject_ms}мс total={}мс",
        t_all.elapsed().as_millis()
    ));
    // Open a short, privacy-scoped learning window. The keyboard hook sends
    // only a boolean ManualEdit signal; the actual edited value is read later
    // from this exact field through Accessibility/UIA (never via clipboard).
    // Snippet bodies are authored templates, not ASR hypotheses: editing a
    // rendered snippet must not silently create a global speech correction.
    if snippet_expanded || text.split_whitespace().next().is_none() {
        close_correction_capture(ctx);
    } else {
        let learning_field_before = if live_inserted {
            field_before_dictation
        } else {
            compact_learning_text(&final_target_actx.field_text)
        };
        *ctx.last_inject.lock() = Some(LastInject {
            text: text.clone(),
            at: Instant::now(),
            target_fp: target_fp.clone(),
            field_id: final_target_actx.field_id.clone(),
            field_role: final_target_actx.field_role.clone(),
            field_subrole: final_target_actx.field_subrole.clone(),
            field_before: learning_field_before,
            manual_edit_at: None,
            pending_observation: None,
        });
        ctx.correction_capture_active.store(true, Ordering::Release);
    }

    let words = text.split_whitespace().count() as u32;
    if words == 0 {
        // Вся диктовка была одиночной командой («с новой строки»/«абзац»):
        // текст после команд — только whitespace ("\n"/"\n\n"). Вставка выше
        // ВЫПОЛНЕНА (inject принимает whitespace clipboard-путём), но запись
        // в историю/статистику и событие transcript с «пустым» текстом — мусор
        // в Dashboard (words==0), поэтому пропускаем. Датасет персонализации
        // пара (аудио ↔ "\n") тоже не учит ничему — не сохраняем.
        dbg_log("финал: текст — только whitespace (команда) — история/transcript пропущены");
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    {
        // P2-8: app_context уже вычислен выше (для тона) — пишем его в history.app,
        // а не пустую строку: бейдж приложения в Истории и Stats.apps_count оживают.
        let conn = ctx.db.lock();
        let _ = db::record_dictation(&conn, &text, &actx.exe, words, ms);
    }
    // Персонализация: сохраняем пару (аудио ↔ текст) в датасет.
    if s.personalize {
        save_sample(ctx, &wav, &text);
    }
    // Убираем за собой уникальный временный WAV этой диктовки (C4).
    let _ = std::fs::remove_file(&wav);
    let _ = ctx.app.emit(
        "transcript",
        serde_json::json!({ "text": text, "ms": ms, "words": words, "seq": my_gen }),
    );
    Ok(())
}

fn insertion_failure_help() -> &'static str {
    if cfg!(target_os = "macos") {
        "Не удалось вставить текст. Разрешите VoxFlow в macOS Privacy & Security -> Accessibility"
    } else if cfg!(windows) {
        "Не удалось вставить текст. Верните фокус в целевое поле и попробуйте способ вставки «Буфер обмена»"
    } else {
        "Не удалось вставить текст. Верните фокус в целевое поле"
    }
}

fn potentially_remote_rewrite(s: &Settings) -> bool {
    let selected_remote = match s.ai_backend.as_str() {
        "gemini" => crate::gemini::available(&s.ai_api_key),
        "openai_compat" => {
            crate::rewrite::configured(s) && !crate::net::is_loopback_base_url(&s.rewrite_base_url)
        }
        _ => false,
    };
    let remote_ollama = s.ai_backend == "ollama"
        && crate::ollama::configured(&s.ollama_url)
        && !crate::net::is_loopback_base_url(&s.ollama_url);
    selected_remote || remote_ollama
}

/// Сохранить пару (аудио 16 кГц ↔ распознанный текст) в датасет персонализации.
fn save_sample(ctx: &EngineCtx, wav: &std::path::Path, text: &str) {
    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d_%H%M%S_%3f").to_string();
    let dest = paths::dataset_dir().join(format!("{stamp}.wav"));
    let audio = match std::fs::copy(wav, &dest) {
        Ok(_) => match paths::set_private_file_permissions(&dest) {
            Ok(()) => dest.to_string_lossy().to_string(),
            Err(e) => {
                let _ = std::fs::remove_file(&dest);
                log::warn!("save_sample permissions: {e}");
                String::new()
            }
        },
        Err(e) => {
            log::warn!("save_sample copy: {e}");
            String::new()
        }
    };
    let ts = now.format("%Y-%m-%d %H:%M:%S").to_string();
    let conn = ctx.db.lock();
    let _ = conn.execute(
        "INSERT INTO samples(ts,audio,text) VALUES(?1,?2,?3)",
        rusqlite::params![ts, audio, text],
    );
}

/// Адаптивный biasing частыми словами — ВЫКЛЮЧЕН (сбивал распознавание), оставлен для будущего.
#[allow(dead_code)]
fn adaptive_prompt(db: &Arc<Mutex<Connection>>) -> Option<String> {
    let texts: Vec<String> = {
        let conn = db.lock();
        let mut out = Vec::new();
        if let Ok(mut stmt) = conn.prepare("SELECT text FROM samples ORDER BY id DESC LIMIT 400") {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                out.extend(rows.flatten());
            }
        }
        if out.is_empty() {
            if let Ok(mut stmt) =
                conn.prepare("SELECT text FROM history ORDER BY id DESC LIMIT 400")
            {
                if let Ok(rows) = stmt.query_map([], |r| r.get::<_, String>(0)) {
                    out.extend(rows.flatten());
                }
            }
        }
        out
    };
    if texts.is_empty() {
        return None;
    }
    let mut freq: HashMap<String, u32> = HashMap::new();
    for t in &texts {
        for w in t.split(|c: char| !c.is_alphanumeric()) {
            if w.chars().count() >= 4 {
                *freq.entry(w.to_lowercase()).or_default() += 1;
            }
        }
    }
    let mut v: Vec<(String, u32)> = freq.into_iter().filter(|(_, c)| *c >= 2).collect();
    if v.is_empty() {
        return None;
    }
    v.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    let top: Vec<String> = v.into_iter().take(40).map(|(w, _)| w).collect();
    Some(top.join(" "))
}

/// Гарантировать загруженный резидентный GigaAM (загрузка ~1 c, один раз).
/// Лок держится на время загрузки — параллельный вызов дождётся и увидит Some.
fn ensure_gigaam(ctx: &EngineCtx, s: &Settings) -> anyhow::Result<()> {
    let mut guard = ctx.gigaam.lock();
    if guard.is_some() {
        return Ok(());
    }
    let dir = paths::gigaam_dir();
    if !crate::gigaam::dir_ready(&dir) {
        return Err(anyhow::Error::new(ModelMissing));
    }
    crate::models::verify_dir_model(crate::models::GIGAAM_NAME)?;
    let t = Instant::now();
    let g = crate::gigaam::GigaAm::load(&dir, s.effective_threads() as usize)?;
    dbg_log(&format!(
        "gigaam: загружен за {} мс",
        t.elapsed().as_millis()
    ));
    *guard = Some(g);
    Ok(())
}

/// Гарантировать загруженный резидентный Parakeet (en/auto) — по образцу
/// ensure_gigaam: ленивая загрузка один раз, лок держится на время загрузки.
/// Модель НЕ скачивается автоматически — только проверка готовности каталога.
fn ensure_parakeet(ctx: &EngineCtx, s: &Settings) -> anyhow::Result<()> {
    let mut guard = ctx.parakeet.lock();
    if guard.is_some() {
        return Ok(());
    }
    let dir = paths::parakeet_dir();
    if !crate::parakeet::dir_ready(&dir) {
        return Err(anyhow::Error::new(ModelMissing));
    }
    crate::models::verify_dir_model(crate::models::PARAKEET_NAME)?;
    let t = Instant::now();
    let p = crate::parakeet::Parakeet::load(&dir, s.effective_threads() as usize)?;
    dbg_log(&format!(
        "parakeet: загружен за {} мс",
        t.elapsed().as_millis()
    ));
    *guard = Some(p);
    Ok(())
}

/// Гарантировать запущенный whisper-server с нужной моделью; вернуть порт.
fn ensure_server(
    ctx: &EngineCtx,
    whisper_dir: &std::path::Path,
    model: &std::path::Path,
    threads: u32,
) -> anyhow::Result<u16> {
    ensure_server_inner(ctx, whisper_dir, model, threads, None)
}

fn ensure_server_cancellable(
    ctx: &EngineCtx,
    whisper_dir: &std::path::Path,
    model: &std::path::Path,
    threads: u32,
    cancel: &AtomicBool,
) -> anyhow::Result<u16> {
    ensure_server_inner(ctx, whisper_dir, model, threads, Some(cancel))
}

fn ensure_server_inner(
    ctx: &EngineCtx,
    whisper_dir: &std::path::Path,
    model: &std::path::Path,
    threads: u32,
    cancel: Option<&AtomicBool>,
) -> anyhow::Result<u16> {
    if cancel
        .map(|flag| flag.load(Ordering::Acquire))
        .unwrap_or(false)
    {
        return Err(anyhow::anyhow!("whisper-server start cancelled"));
    }
    let mut guard = ctx.server.lock();
    if cancel
        .map(|flag| flag.load(Ordering::Acquire))
        .unwrap_or(false)
    {
        return Err(anyhow::anyhow!("whisper-server start cancelled"));
    }
    let need_start = match guard.as_mut() {
        Some(srv) => {
            srv.model.as_path() != model
                || srv.runtime_dir.as_path() != whisper_dir
                || srv.child.try_wait().map(|o| o.is_some()).unwrap_or(true)
        }
        None => true,
    };
    if need_start {
        if let Some(mut old) = guard.take() {
            let _ = old.child.kill();
        }
        let mut last_err: Option<anyhow::Error> = None;
        // reserve_loopback_port already supplies a fresh port. Two attempts cover
        // the narrow bind race without retrying an incompatible CUDA runtime for
        // tens of seconds before the caller can fall back to the CPU package.
        for _ in 0..2 {
            if cancel
                .map(|flag| flag.load(Ordering::Acquire))
                .unwrap_or(false)
            {
                return Err(anyhow::anyhow!("whisper-server start cancelled"));
            }
            let port = asr::reserve_loopback_port()?;
            let ready_timeout =
                whisper_server_ready_timeout(is_accelerated_runtime(ctx, whisper_dir));
            let started = match cancel {
                Some(flag) => asr::start_server_cancellable(
                    whisper_dir,
                    model,
                    port,
                    threads,
                    flag,
                    ready_timeout,
                ),
                None => {
                    asr::start_server_with_timeout(whisper_dir, model, port, threads, ready_timeout)
                }
            };
            match started {
                Ok(srv) => {
                    *guard = Some(srv);
                    return Ok(port);
                }
                Err(error) => {
                    if cancel
                        .map(|flag| flag.load(Ordering::Acquire))
                        .unwrap_or(false)
                    {
                        return Err(error);
                    }
                    if error.downcast_ref::<asr::ServerStartTimeout>().is_some() {
                        return Err(error);
                    }
                    last_err = Some(error);
                }
            }
        }
        return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("whisper-server не поднялся")));
    }
    if let Some(srv) = guard.as_ref() {
        Ok(srv.port)
    } else {
        Err(anyhow::anyhow!("whisper-server не инициализирован"))
    }
}

fn whisper_server_ready_timeout(accelerated: bool) -> Duration {
    if accelerated {
        Duration::from_secs(8)
    } else if cfg!(target_os = "macos") {
        // The bundled arm64 Metal server is ready in <7 s even on the observed
        // slow mounted-DMG path. A broken sidecar should reach CLI fallback in
        // 20 s, not hold the final pipeline for the generic 60 s CPU budget.
        Duration::from_secs(20)
    } else {
        Duration::from_secs(60)
    }
}

fn restart_whisper_server(ctx: &EngineCtx, reason: &str) {
    dbg_log(&format!("whisper-server: stop/restart ({reason})"));
    if let Some(mut old) = ctx.server.lock().take() {
        let _ = old.child.kill();
        let _ = old.child.wait();
    }
}

/// Типизированная ошибка «модель не установлена» — чтобы отличать её от прочих
/// сбоев (микрофон/сервер) и показывать специальное предупреждение «Выберите модель»,
/// а не глотать в общий "error". Матчится через `err.downcast_ref::<ModelMissing>()`.
#[derive(Debug)]
struct ModelMissing;
impl std::fmt::Display for ModelMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Модель не установлена — скачайте её во вкладке «Модель»."
        )
    }
}
impl std::error::Error for ModelMissing {}

fn whisper_model_installed(s: &Settings) -> bool {
    if crate::models::verify_whisper_model_path(&paths::model_path(&s.model)).is_ok() {
        return true;
    }
    if let Ok(rd) = std::fs::read_dir(paths::models_dir()) {
        for entry in rd.flatten() {
            if entry.path().extension().and_then(|x| x.to_str()) == Some("bin")
                && crate::models::verify_whisper_model_path(&entry.path()).is_ok()
            {
                return true;
            }
        }
    }
    false
}

/// true, если выбранному маршруту нечем распознавать. Для быстрых RU/EN моделей
/// допускаем whisper fallback; для остальных языков обязательна whisper-модель.
fn no_model_installed(s: &Settings) -> bool {
    let whisper_ready = || whisper_model_installed(s);
    match local_route(s) {
        LocalRoute::GigaAm => {
            !crate::models::dir_model_ready(crate::models::GIGAAM_NAME) && !whisper_ready()
        }
        LocalRoute::Parakeet => {
            !crate::models::dir_model_ready(crate::models::PARAKEET_NAME) && !whisper_ready()
        }
        LocalRoute::Whisper
            if s.engine == "gigaam"
                && is_auto_language_alias(&s.language)
                && crate::models::dir_model_ready(crate::models::GIGAAM_NAME) =>
        {
            false
        }
        LocalRoute::Whisper => !whisper_ready(),
    }
}

/// Выбрать модель: из настроек, иначе — самая БОЛЬШАЯ установленная *.bin
/// (эвристика «самая мощная»), иначе типизированная ошибка `ModelMissing`.
fn resolve_model(s: &Settings) -> anyhow::Result<std::path::PathBuf> {
    let p = paths::model_path(&s.model);
    if p.exists() {
        match crate::models::verify_whisper_model_path(&p) {
            Ok(()) => return Ok(p),
            Err(error) => log::warn!("выбранная Whisper-модель повреждена: {error:#}"),
        }
    }
    let mut best: Option<(u64, std::path::PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(paths::models_dir()) {
        for entry in rd.flatten() {
            let pp = entry.path();
            if pp.extension().and_then(|x| x.to_str()) == Some("bin") {
                if crate::models::verify_whisper_model_path(&pp).is_err() {
                    continue;
                }
                let sz = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if best.as_ref().map(|(b, _)| sz > *b).unwrap_or(true) {
                    best = Some((sz, pp));
                }
            }
        }
    }
    if let Some((_, pp)) = best {
        log::warn!(
            "модель {} не найдена, fallback → {:?}",
            s.model,
            pp.file_name()
        );
        return Ok(pp);
    }
    Err(anyhow::Error::new(ModelMissing))
}

fn load_dict(conn: &Connection) -> Vec<postprocess::Dict> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT term, replacement FROM dictionary") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(postprocess::Dict {
                term: r.get(0)?,
                replacement: r.get(1)?,
            })
        }) {
            out.extend(rows.flatten());
        }
    }
    out
}

fn load_snippets(conn: &Connection) -> Vec<postprocess::Snippet> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT trigger, content, is_template FROM snippets") {
        if let Ok(rows) = stmt.query_map([], |r| {
            let is_t: i64 = r.get(2)?;
            Ok(postprocess::Snippet {
                trigger: r.get(0)?,
                content: r.get(1)?,
                is_template: is_t != 0,
            })
        }) {
            out.extend(rows.flatten());
        }
    }
    out
}

fn set_status(ctx: &EngineCtx, status: &str) {
    set_status_with_latch(ctx, status, false);
}

fn set_status_with_latch(ctx: &EngineCtx, status: &str, latched: bool) {
    let _ = ctx.app.emit(
        "status",
        serde_json::json!({
            "status": status,
            "seq": ctx.gen.load(Ordering::SeqCst),
            "latched": latched,
        }),
    );
}

fn live_partial_payload(
    text: &str,
    committed: &str,
    volatile: &str,
    seq: u64,
    lang: Option<&'static str>,
) -> serde_json::Value {
    serde_json::json!({
        "text": text,
        "committed": committed,
        "volatile": volatile,
        "seq": seq,
        "lang": lang,
        "processed": true,
    })
}

fn final_preview_payload(text: &str, seq: u64, lang: Option<&'static str>) -> serde_json::Value {
    serde_json::json!({
        "text": text,
        "committed": text,
        "volatile": "",
        "seq": seq,
        "lang": lang,
        "processed": true,
        "final": true,
    })
}

fn settled_partial_payload(
    text: &str,
    committed: &str,
    seq: u64,
    lang: Option<&'static str>,
) -> serde_json::Value {
    serde_json::json!({
        "text": text,
        "committed": committed,
        "volatile": "",
        "seq": seq,
        "lang": lang,
        "processed": true,
        "settled": true,
    })
}

fn emit_final_preview(app: &AppHandle, text: &str, seq: u64, lang: Option<&'static str>) {
    let _ = app.emit("partial", final_preview_payload(text, seq, lang));
}

fn final_rewrite_eligible(
    s: &Settings,
    rewrite_tone: &str,
    smart_active: bool,
    explicit_smart_instruction: bool,
) -> bool {
    if s.verbatim || configured_rewrite_backend(s).is_none() {
        return false;
    }
    match rewrite_tone {
        "verbatim" | "code" => false,
        // Built-in AI context shapes prompts without blocking insertion on LLM.
        // User rules/global smart prompts are the explicit opt-in for sync rewrite.
        "ai" => smart_active && explicit_smart_instruction,
        "" | "neutral" => smart_active && explicit_smart_instruction,
        _ => true,
    }
}

#[cfg(test)]
mod overlay_event_tests {
    use super::*;

    #[test]
    fn live_partial_payload_is_marked_processed() {
        let payload = live_partial_payload("Привет мир", "Привет", "мир", 7, None);

        assert_eq!(payload["text"], "Привет мир");
        assert_eq!(payload["committed"], "Привет");
        assert_eq!(payload["volatile"], "мир");
        assert_eq!(payload["seq"], 7);
        assert_eq!(payload["processed"], true);
        assert!(payload.get("final").is_none());
    }

    #[test]
    fn final_preview_payload_is_marked_and_committed() {
        let payload = final_preview_payload("Исправленный текст.", 42, Some("ru"));

        assert_eq!(payload["text"], "Исправленный текст.");
        assert_eq!(payload["committed"], "Исправленный текст.");
        assert_eq!(payload["volatile"], "");
        assert_eq!(payload["seq"], 42);
        assert_eq!(payload["lang"], "ru");
        assert_eq!(payload["processed"], true);
        assert_eq!(payload["final"], true);
    }

    #[test]
    fn settled_partial_payload_marks_silence_preview() {
        let payload = settled_partial_payload("Готовый текст.", "Готовый текст.", 43, Some("ru"));

        assert_eq!(payload["text"], "Готовый текст.");
        assert_eq!(payload["volatile"], "");
        assert_eq!(payload["seq"], 43);
        assert_eq!(payload["processed"], true);
        assert_eq!(payload["settled"], true);
        assert!(payload.get("final").is_none());
    }
}

fn emit_error(app: &AppHandle, msg: &str) {
    let _ = app.emit("error", serde_json::json!({ "message": msg }));
}

/// Сообщить фронту, какой STT-движок отработал финал и работали ли офлайн.
/// engine — "openai_compat" | "deepgram" | "local"; offline=true только для
/// локального whisper (нет сети). Пилюля показывает индикатор облако/офлайн.
fn emit_stt_mode(app: &AppHandle, engine: &str, offline: bool) {
    let _ = app.emit(
        "stt_mode",
        serde_json::json!({ "engine": engine, "offline": offline }),
    );
}

/// Специальное предупреждение «модель не выбрана/не установлена» — фронт показывает
/// баннер с кнопкой перехода на вкладку «Модель», overlay дублирует кратко.
fn emit_no_model(app: &AppHandle) {
    let _ = app.emit(
        "no_model",
        serde_json::json!({ "message": "Выберите модель во вкладке «Модель»" }),
    );
}

/// Локальный ASR с роутингом по языку (PLAN §2): ru → GigaAM (как раньше),
/// explicit en → Parakeet при установленной модели, auto/прочие языки → Whisper. Общий VAD-гейт тишины для
/// резидентных движков. Возвращает (текст, язык для бейджа overlay).
fn local_asr(
    ctx: &EngineCtx,
    s: &Settings,
    dict: &[postprocess::Dict],
    snippets: &[postprocess::Snippet],
    wav: &std::path::Path,
    samples_16k: &[f32],
    my_gen: u64,
) -> anyhow::Result<(String, Option<&'static str>)> {
    let route = local_route(s);
    if route != LocalRoute::Whisper {
        // Гейт тишины: нет речи → пустой raw → общий norecog-путь (как у гейта
        // уверенности whisper). RNNT/TDT не галлюцинируют как whisper, поэтому
        // отдельного пословного гейта не нужно.
        let t_vad = Instant::now();
        let speech = match ctx.vad_final.lock().as_mut() {
            Some(v) => v.has_speech(samples_16k, 0.5).unwrap_or(true),
            None => true,
        };
        let vad_ms = t_vad.elapsed().as_millis() as u64;
        if !speech {
            dbg_log(&format!(
                "[lat] vad={vad_ms}мс: речи нет — отклонено без ASR"
            ));
            return Ok((String::new(), None));
        }
        match route {
            LocalRoute::GigaAm => match ensure_gigaam(ctx, s) {
                Ok(()) => {
                    let mut guard = ctx.gigaam.lock();
                    if let Some(g) = guard.as_mut() {
                        match local_transcribe_long(&ctx.vad_final, samples_16k, &mut |seg| {
                            g.transcribe(seg)
                        }) {
                            Ok(t) => {
                                let st = g.last_stats;
                                emit_stt_mode(&ctx.app, "gigaam", false);
                                dbg_log(&format!(
                                    "[lat] vad={vad_ms}мс gigaam: audio={}мс frontend={}мс encoder={}мс decoder={}мс asr={}мс",
                                    st.audio_ms, st.frontend_ms, st.encoder_ms, st.decoder_ms, st.total_ms
                                ));
                                // RNNT почти не повторяется, но dedup дешёвый —
                                // единая анти-повторная защита всех движков.
                                return Ok((postprocess::dedup_repeated_ngrams(&t), Some("ru")));
                            }
                            Err(e) => log::warn!("gigaam ошибка: {e:#}; откат на whisper"),
                        }
                    }
                }
                Err(e) => {
                    // Модели GigaAM нет и whisper-фолбэка тоже — честная ошибка
                    // «выберите модель»; иначе тихо уходим на whisper.
                    if e.downcast_ref::<ModelMissing>().is_some() && no_model_installed(s) {
                        return Err(e);
                    }
                    log::warn!("gigaam недоступен ({e}); откат на whisper");
                }
            },
            LocalRoute::Parakeet => match ensure_parakeet(ctx, s) {
                Ok(()) => {
                    let mut guard = ctx.parakeet.lock();
                    if let Some(p) = guard.as_mut() {
                        match local_transcribe_long(&ctx.vad_final, samples_16k, &mut |seg| {
                            p.transcribe(seg)
                        }) {
                            Ok(t) => {
                                let st = p.last_stats;
                                dbg_log(&format!(
                                    "[lat] vad={vad_ms}мс parakeet: audio={}мс frontend={}мс encoder={}мс decoder={}мс asr={}мс",
                                    st.audio_ms, st.frontend_ms, st.encoder_ms, st.decoder_ms, st.total_ms
                                ));
                                let lang = detect_lang_label(&t);
                                if s.language == "auto"
                                    && lang == Some("ru")
                                    && crate::gigaam::dir_ready(&paths::gigaam_dir())
                                {
                                    match ensure_gigaam(ctx, s) {
                                        Ok(()) => {
                                            let mut g_guard = ctx.gigaam.lock();
                                            if let Some(g) = g_guard.as_mut() {
                                                match local_transcribe_long(
                                                    &ctx.vad_final,
                                                    samples_16k,
                                                    &mut |seg| g.transcribe(seg),
                                                ) {
                                                    Ok(g_text) if prefer_gigaam_for_auto(&t, &g_text) => {
                                                        emit_stt_mode(&ctx.app, "gigaam", false);
                                                        dbg_log(&format!(
                                                            "auto: выбран GigaAM вместо Parakeet (parakeet_len={}, gigaam_len={})",
                                                            t.chars().count(),
                                                            g_text.chars().count()
                                                        ));
                                                        return Ok((
                                                            postprocess::dedup_repeated_ngrams(&g_text),
                                                            Some("ru"),
                                                        ));
                                                    }
                                                    Ok(_) => {}
                                                    Err(e) => log::warn!(
                                                        "auto: GigaAM после Parakeet недоступен: {e:#}"
                                                    ),
                                                }
                                            }
                                        }
                                        Err(e) => log::warn!(
                                            "auto: GigaAM после Parakeet недоступен ({e})"
                                        ),
                                    }
                                }
                                emit_stt_mode(&ctx.app, "parakeet", false);
                                return Ok((postprocess::dedup_repeated_ngrams(&t), lang));
                            }
                            Err(e) => log::warn!("parakeet ошибка: {e:#}; откат на whisper"),
                        }
                    }
                }
                Err(e) => log::warn!("parakeet недоступен ({e}); откат на whisper"),
            },
            LocalRoute::Whisper => unreachable!(),
        }
    }
    // Explicit en without Parakeet uses Whisper and shows a one-time hint.
    // подсказка, что с Parakeet будет лучше.
    if route == LocalRoute::Whisper
        && s.engine == "gigaam"
        && matches!(
            s.language.trim().to_ascii_lowercase().as_str(),
            "en" | "english"
        )
    {
        hint_parakeet_once(&ctx.app);
    }
    if route == LocalRoute::Whisper
        && s.engine == "gigaam"
        && is_auto_language_alias(&s.language)
        && !whisper_model_installed(s)
        && crate::gigaam::dir_ready(&paths::gigaam_dir())
    {
        let t_vad = Instant::now();
        let speech = match ctx.vad_final.lock().as_mut() {
            Some(v) => v.has_speech(samples_16k, 0.5).unwrap_or(true),
            None => true,
        };
        let vad_ms = t_vad.elapsed().as_millis() as u64;
        if !speech {
            dbg_log(&format!(
                "[lat] vad={vad_ms}мс: речи нет — отклонено без ASR"
            ));
            return Ok((String::new(), None));
        }
        match ensure_gigaam(ctx, s) {
            Ok(()) => {
                let mut guard = ctx.gigaam.lock();
                if let Some(g) = guard.as_mut() {
                    match local_transcribe_long(&ctx.vad_final, samples_16k, &mut |seg| {
                        g.transcribe(seg)
                    }) {
                        Ok(t) => {
                            let st = g.last_stats;
                            emit_stt_mode(&ctx.app, "gigaam", false);
                            dbg_log(&format!(
                                "[lat] vad={vad_ms}мс auto-fallback gigaam: audio={}мс frontend={}мс encoder={}мс decoder={}мс asr={}мс",
                                st.audio_ms, st.frontend_ms, st.encoder_ms, st.decoder_ms, st.total_ms
                            ));
                            return Ok((postprocess::dedup_repeated_ngrams(&t), Some("ru")));
                        }
                        Err(e) => log::warn!("auto-fallback GigaAM ошибка: {e:#}"),
                    }
                }
            }
            Err(e) => log::warn!("auto-fallback GigaAM недоступен ({e})"),
        }
    }
    let Some(whisper_text) = local_transcribe_guarded(ctx, s, dict, snippets, wav, my_gen)? else {
        return Ok((String::new(), None));
    };
    if s.language == "auto"
        && s.engine == "gigaam"
        && crate::gigaam::dir_ready(&paths::gigaam_dir())
        && should_probe_gigaam_for_auto(&whisper_text)
    {
        match ensure_gigaam(ctx, s) {
            Ok(()) => {
                let mut guard = ctx.gigaam.lock();
                if let Some(g) = guard.as_mut() {
                    match local_transcribe_long(&ctx.vad_final, samples_16k, &mut |seg| {
                        g.transcribe(seg)
                    }) {
                        Ok(t) if prefer_gigaam_for_auto(&whisper_text, &t) => {
                            emit_stt_mode(&ctx.app, "gigaam", false);
                            dbg_log(&format!(
                                "auto: выбран GigaAM вместо whisper auto (whisper_len={}, gigaam_len={})",
                                whisper_text.chars().count(),
                                t.chars().count()
                            ));
                            return Ok((postprocess::dedup_repeated_ngrams(&t), Some("ru")));
                        }
                        Ok(_) => {}
                        Err(e) => log::warn!("auto: GigaAM final fallback ошибка: {e:#}"),
                    }
                }
            }
            Err(e) => log::warn!("auto: GigaAM final fallback недоступен ({e})"),
        }
    }
    let lang = detect_lang_label(&whisper_text);
    Ok((whisper_text, lang))
}

/// Разовая (на сессию) подсказка для explicit en без установленного Parakeet:
/// фолбэк-whisper работает, но предлагаем модель получше. Баннер no_model
/// на фронте уже умеет показывать произвольный message с кнопкой на «Модель».
fn hint_parakeet_once(app: &AppHandle) {
    static SHOWN: AtomicBool = AtomicBool::new(false);
    if SHOWN.swap(true, Ordering::SeqCst) {
        return;
    }
    let _ = app.emit(
        "no_model",
        serde_json::json!({
            "message": "Для английского установите модель «Parakeet TDT v3» во вкладке «Модель» — точнее и с живыми партиалами"
        }),
    );
}

/// Финал локального резидентного движка с разметкой: VAD делит запись на фразы
/// (тишина >=600 мс), фразы длиннее 25 c дорезаются по ближайшей тишине (лимит
/// pos_emb GigaAM; Parakeet такие куски тоже переваривает). Длинная пауза между
/// фразами -> абзац ("\n\n"); переговорённая заново фраза заменяет предыдущую
/// (is_restatement) — та же логика, что в живой петле. `transcribe` — замыкание
/// конкретного движка (так auto-маршрут решает судьбу каждого сегмента сам).
/// pub(crate): используется финалом и headless-селфтестами (lib.rs).
pub(crate) fn local_transcribe_long<F>(
    vad: &Arc<Mutex<Option<crate::vad::SileroVad>>>,
    samples: &[f32],
    transcribe: &mut F,
) -> anyhow::Result<String>
where
    F: FnMut(&[f32]) -> anyhow::Result<String>,
{
    const SIL_SPLIT: usize = 600 * 16; // межфразная тишина
    const MAX_SEG: usize = 25 * 16000;
    const PAD: usize = 4800; // 300 мс запас вокруг речи

    let chunk = crate::vad::CHUNK;
    // Карта речи по 512-чанкам.
    let mut speech: Vec<bool> = Vec::with_capacity(samples.len() / chunk + 1);
    {
        let mut vg = vad.lock();
        if let Some(v) = vg.as_mut() {
            v.reset();
            for c in samples.chunks(chunk) {
                speech.push(v.process_chunk(c).unwrap_or(1.0) >= 0.35);
            }
            v.reset();
        }
    }
    if speech.is_empty() || !speech.iter().any(|&x| x) {
        // VAD недоступен/речи не нашёл — поведение как раньше: одним куском
        // (короткое) либо жёсткими срезами по 25 c.
        if samples.len() <= MAX_SEG {
            return transcribe(samples);
        }
        let mut parts = Vec::new();
        let mut start = 0usize;
        while start < samples.len() {
            let cut = (start + MAX_SEG).min(samples.len());
            let t = transcribe(&samples[start..cut])?;
            if !t.trim().is_empty() {
                parts.push(t.trim().to_string());
            }
            start = cut;
        }
        return Ok(parts.join(" "));
    }

    // Фразы: речевые промежутки, разделённые тишиной >= SIL_SPLIT.
    struct Unit {
        start: usize,
        end: usize,
        gap_before: usize,
    }
    let n = speech.len();
    let mut units: Vec<Unit> = Vec::new();
    let mut prev_end_chunk: Option<usize> = None;
    let mut i = 0usize;
    while i < n {
        if !speech[i] {
            i += 1;
            continue;
        }
        let s0 = i;
        let mut last_voiced = i;
        let mut j = i + 1;
        while j < n {
            if speech[j] {
                last_voiced = j;
                j += 1;
                continue;
            }
            let mut k = j;
            while k < n && !speech[k] {
                k += 1;
            }
            if (k - j) * chunk >= SIL_SPLIT {
                break;
            }
            j = k;
        }
        let gap_before = prev_end_chunk
            .map(|pe| s0.saturating_sub(pe) * chunk)
            .unwrap_or(0);
        units.push(Unit {
            start: s0 * chunk,
            end: ((last_voiced + 1) * chunk).min(samples.len()),
            gap_before,
        });
        prev_end_chunk = Some(last_voiced + 1);
        i = j.max(last_voiced + 1);
    }

    // Транскрипция фраз: запас PAD по краям; >25 c — жёсткая дорезка по тишине.
    let mut segs: Vec<(bool, String)> = Vec::new();
    for u in &units {
        let s0 = u.start.saturating_sub(PAD);
        let e0 = (u.end + PAD).min(samples.len());
        let mut texts: Vec<String> = Vec::new();
        let mut start = s0;
        while start < e0 {
            let end_limit = (start + MAX_SEG).min(e0);
            let mut cut = end_limit;
            if end_limit < e0 {
                let from_chunk = start / chunk + 1;
                let to_chunk = (end_limit / chunk).min(speech.len());
                for c in (from_chunk..to_chunk).rev() {
                    if !speech.get(c).copied().unwrap_or(true) {
                        cut = c * chunk;
                        break;
                    }
                }
            }
            let t = transcribe(&samples[start..cut])?;
            if !t.trim().is_empty() {
                texts.push(t.trim().to_string());
            }
            start = cut;
        }
        let t = texts.join(" ");
        if t.is_empty() {
            continue;
        }
        let para = gap_starts_paragraph(!segs.is_empty(), u.gap_before);
        push_dictation_segment(&mut segs, para, t);
    }
    Ok(render_segments(&segs))
}

fn gap_starts_paragraph(has_previous_segment: bool, gap_samples: usize) -> bool {
    has_previous_segment && gap_samples >= PARAGRAPH_GAP_SAMPLES
}

#[cfg(test)]
mod seg_tests {
    use super::*;

    #[test]
    fn final_local_asr_reuses_the_compacted_audio_written_to_wav() {
        let trimmed = vec![1.0_f32; 32];
        let speech_compacted = vec![2.0_f32; 8];

        let selected = final_local_asr_samples(&trimmed, &speech_compacted);

        assert_eq!(selected, speech_compacted.as_slice());
        assert!(std::ptr::eq(selected.as_ptr(), speech_compacted.as_ptr()));
    }

    #[test]
    fn compacted_final_audio_keeps_paragraph_gap_reachable() {
        let padded_eight_second_gap = PARAGRAPH_GAP_SAMPLES - FINAL_ASR_PAD_SAMPLES * 2;

        assert_eq!(
            compacted_final_asr_join_silence(padded_eight_second_gap),
            PARAGRAPH_GAP_SAMPLES
        );
        assert_eq!(
            compacted_final_asr_join_silence(padded_eight_second_gap - 1),
            FINAL_ASR_JOIN_SILENCE_SAMPLES
        );
        assert!(gap_starts_paragraph(
            true,
            compacted_final_asr_join_silence(padded_eight_second_gap)
        ));
    }

    fn live_state_with_draft(mode: &str, draft: &str) -> LiveState {
        let context = crate::app_context::AppContext {
            exe: "editor.exe".into(),
            title: "Editor".into(),
            window_id: "window=1".into(),
            category: "work".into(),
            field_role: "UIAEdit".into(),
            field_subrole: "textpattern".into(),
            field_id: "field=1".into(),
            field_text: draft.into(),
            selected_text: String::new(),
        };
        let injected = if mode == "always" {
            draft.to_string()
        } else {
            String::new()
        };
        let committed = if mode == "auto" {
            draft.to_string()
        } else {
            String::new()
        };
        LiveState {
            stream_mode: mode.into(),
            injected: Arc::new(Mutex::new(injected)),
            committed: Arc::new(Mutex::new(committed)),
            abort: Arc::new(AtomicBool::new(false)),
            start_fp: context.target_fingerprint(),
        }
    }

    fn pending_slot(generation: u64, mode: &str, draft: &str) -> Option<PendingLiveDraft> {
        Some(PendingLiveDraft {
            generation,
            live: live_state_with_draft(mode, draft),
        })
    }

    #[test]
    fn rapid_start_claims_old_live_draft_exactly_once() {
        let mut slot = pending_slot(7, "always", "старый черновик");

        let inherited = take_pending_live_draft_from_slot(&mut slot, 7);

        assert!(inherited.is_some(), "new Start must own the old draft");
        assert!(slot.is_none());
        assert!(
            take_pending_live_draft_from_slot(&mut slot, 7).is_none(),
            "the stale detached final must not regain ownership"
        );
    }

    #[test]
    fn escape_claims_detached_live_draft_exactly_once() {
        let mut slot = pending_slot(11, "auto", "стабильный префикс");

        let cancelled = take_pending_live_draft_from_slot(&mut slot, 11);

        assert!(cancelled.is_some(), "Esc must own the physical draft");
        assert!(
            take_pending_live_draft_from_slot(&mut slot, 11).is_none(),
            "a later final must observe the cancelled ownership token"
        );
    }

    #[test]
    fn successful_final_clears_token_before_next_start_can_erase_it() {
        let mut slot = pending_slot(13, "always", "черновик");
        let live = slot.as_ref().expect("registered draft").live.clone();

        // Final reconciliation succeeded while commit locks are held.
        *live.injected.lock() = "готовый текст".into();
        let terminal = take_pending_live_draft_from_slot(&mut slot, 13);

        assert!(terminal.is_some());
        assert_eq!(&*live.injected.lock(), "готовый текст");
        assert!(
            take_pending_live_draft_from_slot(&mut slot, 13).is_none(),
            "the next Start must not backspace confirmed final text"
        );
    }

    #[test]
    fn multiline_paste_failure_does_not_leave_a_double_erase_ledger() {
        let mut slot = pending_slot(17, "always", "живой черновик");
        let live = slot.as_ref().expect("registered draft").live.clone();

        // The first operation (draft -> empty) succeeded, while the separate
        // multiline clipboard paste failed. Runtime keeps the terminal token,
        // but the physical ledger must already say that nothing remains.
        *live.injected.lock() = String::new();

        assert!(slot.is_some());
        assert!(!live.live_inserted());
        let inherited = take_pending_live_draft_from_slot(&mut slot, 17)
            .expect("next Start may retire the empty token");
        assert!(
            !inherited.live.live_inserted(),
            "no second backspace operation may be scheduled"
        );
    }

    #[test]
    fn rejected_empty_final_erases_live_draft_and_cannot_reach_history() {
        let decision = decide_final_pipeline(false, true);
        let mut erase_calls = 0;
        let mut commit_or_history_entries = 0;

        match decision {
            FinalPipelineDecision::Reject { erase_live_draft } => {
                if erase_live_draft {
                    erase_calls += 1;
                }
            }
            FinalPipelineDecision::Continue => commit_or_history_entries += 1,
        }

        assert_eq!(
            erase_calls, 1,
            "the speculative physical draft must be erased"
        );
        assert_eq!(
            commit_or_history_entries, 0,
            "a rejected final must not enter the commit/history pipeline"
        );
    }

    #[test]
    fn restatement_replaces_similar_neighbor() {
        assert!(is_restatement(
            "Поставь мою песню лучше будет работать",
            "Поставь мою музыку лучше будет работать"
        ));
        assert!(!is_restatement("Я пошёл домой", "Завтра будет дождь"));
        assert!(!is_restatement("да", "да")); // короткие не трогаем
    }

    #[test]
    fn segments_render_paragraphs() {
        let segs = vec![
            (false, "Первая фраза.".to_string()),
            (false, "Вторая рядом.".to_string()),
            (true, "Новый абзац.".to_string()),
        ];
        assert_eq!(
            render_segments(&segs),
            "Первая фраза. Вторая рядом.\n\nНовый абзац."
        );
    }

    #[test]
    fn short_pause_continuation_soft_joins_segments() {
        let mut segs = Vec::new();
        push_dictation_segment(&mut segs, false, "Я немного остановился.".to_string());
        push_dictation_segment(
            &mut segs,
            false,
            "А он начал новое предложение.".to_string(),
        );

        assert_eq!(
            render_segments(&segs),
            "Я немного остановился, а он начал новое предложение."
        );
    }

    #[test]
    fn real_sentence_boundary_survives_segment_join() {
        let mut segs = Vec::new();
        push_dictation_segment(&mut segs, false, "Готово.".to_string());
        push_dictation_segment(&mut segs, false, "Следующая тема.".to_string());

        assert_eq!(render_segments(&segs), "Готово. Следующая тема.");
    }

    #[test]
    fn paragraph_boundary_survives_segment_join() {
        let mut segs = Vec::new();
        push_dictation_segment(&mut segs, false, "Первый блок.".to_string());
        push_dictation_segment(&mut segs, true, "А это новый абзац.".to_string());

        assert_eq!(render_segments(&segs), "Первый блок.\n\nА это новый абзац.");
    }

    #[test]
    fn previous_open_context_lowers_only_clear_continuations() {
        assert_eq!(
            continue_from_previous_context(
                "А он начал новое предложение",
                Some("я немного остановился"),
                "casual",
            ),
            " а он начал новое предложение"
        );
        assert_eq!(
            continue_from_previous_context(
                "Следующая тема",
                Some("я немного остановился"),
                "casual",
            ),
            "Следующая тема"
        );
        assert_eq!(
            continue_from_previous_context("А новый абзац", Some("Готово."), "casual"),
            "А новый абзац"
        );
        assert_eq!(
            continue_from_previous_context("А новый абзац", Some("Текст"), "formal"),
            "А новый абзац"
        );
    }

    #[test]
    fn grammatically_unfinished_context_continues_without_a_cue_word() {
        assert_eq!(
            continue_from_previous_context(
                "Сделал это завтра",
                Some("Я хотел, чтобы ты"),
                "casual",
            ),
            " сделал это завтра"
        );
        assert_eq!(
            continue_from_previous_context(
                "Finished the release",
                Some("I asked that you"),
                "work",
            ),
            " finished the release"
        );
        assert_eq!(
            continue_from_previous_context(
                "Подготовил документ",
                Some("Я попросил, чтобы ты"),
                "formal",
            ),
            " подготовил документ"
        );
        assert_eq!(
            continue_from_previous_context(
                "Сформулировал критерии",
                Some("Сделай так, чтобы ты"),
                "ai",
            ),
            " сформулировал критерии"
        );
        assert_eq!(
            continue_from_previous_context(
                "Это продолжение",
                Some("Я хотел, чтобы ты\n"),
                "casual",
            ),
            "Это продолжение"
        );
        assert_eq!(
            continue_from_previous_context(", наверное завтра", Some("Я думаю, что"), "casual"),
            ", наверное завтра"
        );
        assert_eq!(
            continue_from_previous_context("Новая мысль", None, "casual"),
            "Новая мысль"
        );
        assert_eq!(
            continue_from_previous_context("Новая мысль", Some("   "), "casual"),
            "Новая мысль"
        );
        assert_eq!(
            continue_from_previous_context("Москву вчера", Some("Я приехал в"), "casual"),
            " Москву вчера"
        );
        assert_eq!(
            continue_from_previous_context("Новой вкладке", Some("Открой документ в"), "work"),
            " новой вкладке"
        );
        assert_eq!(
            continue_from_previous_context("Нижний Новгород", Some("Я приехал в"), "casual"),
            " Нижний Новгород"
        );
    }

    #[test]
    fn unfinished_ending_gate_respects_exact_modes_and_punctuation_setting() {
        let defaults = Settings::default();
        assert!(should_preserve_unfinished_ending(
            &Settings {
                auto_punct: true,
                ..defaults.clone()
            },
            "neutral",
            false,
        ));
        assert!(!should_preserve_unfinished_ending(
            &Settings {
                auto_punct: false,
                ..defaults.clone()
            },
            "neutral",
            false,
        ));
        assert!(!should_preserve_unfinished_ending(
            &Settings {
                auto_punct: true,
                verbatim: true,
                ..defaults.clone()
            },
            "neutral",
            false,
        ));
        for tone in ["code", "verbatim"] {
            assert!(!should_preserve_unfinished_ending(
                &Settings {
                    auto_punct: true,
                    ..defaults.clone()
                },
                tone,
                false,
            ));
        }
        assert!(!should_preserve_unfinished_ending(
            &Settings {
                auto_punct: true,
                ..defaults
            },
            "neutral",
            true,
        ));
    }

    #[test]
    fn ellipsis_keeps_context_open_but_real_period_closes_it() {
        assert!(previous_context_is_closed("Я ещё думаю…"));
        assert!(!previous_context_is_closed("Я хотел сказать, что…"));
        assert!(!previous_context_is_closed("Я хотел сказать, что."));
        assert!(previous_context_is_closed("Релиз готов."));
        assert!(previous_context_is_closed("Релиз готов!"));
        assert!(previous_context_is_closed("Строка завершена\n"));
    }

    #[test]
    fn live_context_uses_the_pre_dictation_field_not_its_own_draft() {
        let context = crate::app_context::AppContext {
            exe: "editor.exe".into(),
            title: "Editor".into(),
            window_id: "window=1".into(),
            category: "work".into(),
            field_role: "UIAEdit".into(),
            field_subrole: "textpattern".into(),
            field_id: "field=1".into(),
            field_text: "Я хотел, чтобы ты Сделал это".into(),
            selected_text: String::new(),
        };
        assert_eq!(
            visible_previous_context_tail(&context, "Я хотел, чтобы ты", true),
            Some("Я хотел, чтобы ты".into())
        );
        assert_eq!(visible_previous_context_tail(&context, "", true), None);
        assert_eq!(
            visible_previous_context_tail(&context, "ignored", false),
            Some("Я хотел, чтобы ты Сделал это".into())
        );
        let bounded = visible_previous_context_tail(&context, "Я хотел, чтобы ты\n", true)
            .expect("newline boundary");
        assert!(bounded.ends_with('\n'));
        assert_eq!(
            continue_from_previous_context("Новая тема", Some(&bounded), "casual"),
            "Новая тема"
        );
    }

    #[test]
    fn dictation_memory_never_crosses_fields_or_unidentified_controls() {
        let context = crate::app_context::AppContext {
            exe: "browser.exe".into(),
            title: "Form".into(),
            window_id: "window=1".into(),
            category: "work".into(),
            field_role: "UIAEdit".into(),
            field_subrole: "textpattern".into(),
            field_id: "field=A".into(),
            field_text: String::new(),
            selected_text: String::new(),
        };
        let memory = DictationMemory {
            target_fp: dictation_memory_target(&context),
            ..DictationMemory::default()
        };
        assert!(target_matches_memory(&memory, &context));

        let mut other = context.clone();
        other.field_id = "field=B".into();
        assert!(!target_matches_memory(&memory, &other));

        other.field_id.clear();
        assert!(!target_matches_memory(&memory, &other));
        assert!(dictation_memory_target(&other).is_none());
    }

    #[test]
    fn final_gate_accepts_an_explicit_line_break_without_text() {
        assert!(final_text_is_insertable("\n"));
        assert!(final_text_is_insertable("\n\n"));
        assert!(final_text_is_insertable("готово"));
        assert!(!final_text_is_insertable("   \t"));
    }

    #[test]
    fn remembered_line_break_is_a_hard_sentence_boundary() {
        let context = crate::app_context::AppContext {
            exe: "editor.exe".into(),
            title: "Editor".into(),
            window_id: "window=1".into(),
            category: "work".into(),
            field_role: "UIAEdit".into(),
            field_subrole: "textpattern".into(),
            field_id: "field=A".into(),
            field_text: String::new(),
            selected_text: String::new(),
        };
        let mut memory = DictationMemory::default();
        remember_dictation_context_in(&mut memory, &context, "Я хотел, чтобы ты", false);
        remember_dictation_context_in(&mut memory, &context, "\n", false);

        let boundary = dictation_memory_item_context(
            memory.recent.back().expect("remembered newline boundary"),
        );
        assert_eq!(boundary, "\n");
        assert_eq!(
            continue_from_previous_context("Новая тема", Some(&boundary), "casual"),
            "Новая тема"
        );
    }

    #[test]
    fn spoken_period_stays_a_boundary_after_existing_field_text() {
        let context = crate::app_context::AppContext {
            exe: "editor.exe".into(),
            title: "Editor".into(),
            window_id: "window=1".into(),
            category: "work".into(),
            field_role: "UIAEdit".into(),
            field_subrole: "textpattern".into(),
            field_id: "field=A".into(),
            field_text: String::new(),
            selected_text: String::new(),
        };
        let mut memory = DictationMemory::default();
        remember_dictation_context_in(&mut memory, &context, "Я хотел сказать, что.", true);

        let boundary = previous_context_with_memory_boundary_in(
            &memory,
            &context,
            Some("Привет. Я хотел сказать, что.".into()),
        )
        .expect("visible field tail");
        assert!(boundary.ends_with('\n'));
        assert_eq!(
            continue_from_previous_context("Новая тема", Some(&boundary), "casual"),
            "Новая тема"
        );

        let unrelated = previous_context_with_memory_boundary_in(
            &memory,
            &context,
            Some("Привет. Совсем другой текст.".into()),
        );
        assert_eq!(unrelated.as_deref(), Some("Привет. Совсем другой текст."));
        assert!(!visible_tail_matches_memory_item(
            "контекстчто.",
            "что.",
            false,
        ));

        let long_spoken_sentence = format!(
            "{}Я хотел сказать, что.",
            "очень длинный контекст ".repeat(30)
        );
        remember_dictation_context_in(&mut memory, &context, &long_spoken_sentence, true);
        let visible_long =
            sentence_context_tail(&format!("Старый префикс. {long_spoken_sentence}"), 600);
        let long_boundary =
            previous_context_with_memory_boundary_in(&memory, &context, Some(visible_long))
                .expect("long visible field tail");
        assert!(long_boundary.ends_with('\n'));
    }

    #[test]
    fn paragraph_gap_keeps_short_continuation_together() {
        assert!(!gap_starts_paragraph(true, 3 * 16000));
        assert!(!gap_starts_paragraph(true, 4 * 16000));
        assert!(gap_starts_paragraph(true, 8 * 16000));
        assert!(!gap_starts_paragraph(false, 10 * 16000));
    }

    #[test]
    fn live_partial_is_cleaned_as_one_preview() {
        let s = Settings::default();
        let (committed, volatile, full) =
            clean_live_partial("ну короче привет", "мир", &s, &[], &[], &[]);

        assert_eq!(committed, "Привет");
        assert_eq!(volatile, "мир");
        assert_eq!(full, "Привет мир");
    }

    #[test]
    fn exact_snippet_live_preview_matches_the_final_body_verbatim() {
        let s = Settings::default();
        let snippets = vec![postprocess::Snippet {
            trigger: "/sig".into(),
            content: "cat\n  indented signature".into(),
            is_template: false,
        }];
        let corrections = vec![postprocess::Correction {
            wrong: "cat".into(),
            right: "dog".into(),
        }];

        let (_, volatile, live_full) =
            clean_live_partial("", "/sig", &s, &[], &snippets, &corrections);
        let final_body = postprocess::expand_matching_snippet("/sig", &snippets)
            .expect("exact snippet must expand");

        assert_eq!(live_full, final_body);
        assert_eq!(volatile, final_body);
        assert_eq!(live_full, "cat\n  indented signature");
    }

    #[test]
    fn flatten_breaks_for_keyboard() {
        assert_eq!(flatten_breaks("а\n\nб  в"), "а б в");
    }

    #[test]
    fn auto_prefers_gigaam_when_whisper_turns_russian_into_short_latin() {
        assert!(prefer_gigaam_for_auto(
            "Państwo, unze",
            "Пользователь говорит обычный русский текст"
        ));
        assert!(prefer_gigaam_for_auto("After", "Исправь это пожалуйста"));
        assert!(!prefer_gigaam_for_auto(
            "please update the prompt",
            "Пожалуйста обнови промпт"
        ));
        assert!(!prefer_gigaam_for_auto(
            "Подвяжи к боту ИИ и обучи его на шаблонах",
            "Подключи бота и добавь шаблоны для обучения"
        ));
        assert!(!should_probe_gigaam_for_auto(
            "Подвяжи к боту ИИ и обучи его на шаблонах"
        ));
        assert!(should_probe_gigaam_for_auto("After"));
        assert!(should_probe_gigaam_for_auto("Коротко"));
        assert!(!should_probe_gigaam_for_auto("Обнови README.md сегодня"));
    }

    #[test]
    fn local_route_respects_explicit_whisper_engine() {
        let mut s = Settings {
            engine: "whisper_server".to_string(),
            language: "auto".to_string(),
            ..Settings::default()
        };
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Whisper);

        s.language = "en".to_string();
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Whisper);

        s.language = "ru".to_string();
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Whisper);

        s.engine = "gigaam".to_string();
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::GigaAm);
    }

    #[test]
    fn auto_language_stays_on_multilingual_whisper_even_with_parakeet_installed() {
        let mut s = Settings {
            engine: "gigaam".to_string(),
            language: "auto".to_string(),
            ..Settings::default()
        };
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Whisper);
        assert_eq!(local_route_with_parakeet(&s, false), LocalRoute::Whisper);

        s.language = "multi".to_string();
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Whisper);

        s.language = "*".to_string();
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Whisper);

        s.language = "en".to_string();
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Parakeet);
    }

    #[test]
    fn cloud_fallback_preserves_selected_local_route() {
        let mut s = Settings {
            engine: "gigaam".to_string(),
            language: "ru".to_string(),
            ..Settings::default()
        };
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::GigaAm);

        s.language = "auto".to_string();
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Whisper);
        assert_eq!(local_route_with_parakeet(&s, false), LocalRoute::Whisper);

        s.engine = "whisper_server".to_string();
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Whisper);
    }

    #[test]
    fn preview_only_mode_skips_heavy_whisper_but_opt_in_modes_allow_it() {
        assert!(!whisper_preview_requested("never"));
        assert!(whisper_preview_requested("auto"));
        assert!(whisper_preview_requested("always"));
    }

    #[test]
    fn resident_preview_probe_never_waits_for_model_initialization() {
        let model = Arc::new(Mutex::new(Some(7u8)));
        assert!(resident_model_ready(&model));
        let loading = model.lock();
        assert!(!resident_model_ready(&model));
        drop(loading);
        *model.lock() = None;
        assert!(!resident_model_ready(&model));
    }

    #[test]
    fn stale_generation_is_rejected_before_expensive_fallbacks() {
        assert!(generation_is_current(9, 9));
        assert!(!generation_is_current(10, 9));
    }

    #[test]
    fn macos_capture_requires_insertion_permission() {
        assert!(insertion_permission_blocks_capture(true, false));
        assert!(!insertion_permission_blocks_capture(true, true));
        assert!(!insertion_permission_blocks_capture(false, false));
    }

    #[test]
    fn quick_double_tap_candidate_does_not_flash_false_norecog() {
        assert!(!should_emit_norecog(0));
        assert!(!should_emit_norecog(180));
        assert!(!should_emit_norecog(499));
        assert!(should_emit_norecog(500));
    }

    #[test]
    fn busy_partial_never_blocks_the_next_engine_command() {
        let join = std::thread::spawn(|| std::thread::sleep(Duration::from_millis(200)));
        let started = Instant::now();
        finish_partial_without_blocking(join, PartialKind::Local);
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn server_start_deadline_is_tight_on_macos_but_keeps_cpu_budget_elsewhere() {
        assert_eq!(whisper_server_ready_timeout(true), Duration::from_secs(8));
        let expected = if cfg!(target_os = "macos") { 20 } else { 60 };
        assert_eq!(
            whisper_server_ready_timeout(false),
            Duration::from_secs(expected)
        );
    }

    #[test]
    fn whisper_base_prompt_covers_auto_aliases() {
        assert_eq!(
            whisper_base_prompt("ru"),
            Some(postprocess::DEFAULT_RU_PROMPT)
        );
        for lang in ["auto", "all", "any", "multi", "multilingual", "*"] {
            let prompt = whisper_base_prompt(lang).expect("multilingual prompt");
            assert!(prompt.contains("language switches"));
            assert!(prompt.contains("do not add a final period"));
            assert_eq!(whisper_language_arg(lang), "auto");
        }
        assert!(postprocess::DEFAULT_RU_PROMPT.contains("Окончание записи"));
        assert!(crate::ollama::SYSTEM_PROMPT.contains("Окончание записи"));
        assert!(crate::ollama::SYSTEM_PROMPT.contains("Грамматически незавершённую"));
        assert!(whisper_base_prompt("en").is_none());
        assert_eq!(whisper_language_arg("en"), "en");
    }
}

/// Локальное распознавание whisper (server → cli fallback).
fn local_transcribe_guarded(
    ctx: &EngineCtx,
    s: &Settings,
    dict: &[postprocess::Dict],
    snippets: &[postprocess::Snippet],
    wav: &std::path::Path,
    expected_gen: u64,
) -> anyhow::Result<Option<String>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if !final_generation_is_current(ctx, expected_gen) {
            dbg_log("финал: поколение устарело в ожидании asr_lock — Whisper пропущен");
            return Ok(None);
        }
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(50));
        if let Some(_g) = ctx.asr_lock.try_lock_for(wait) {
            if !final_generation_is_current(ctx, expected_gen) {
                dbg_log("финал: поколение устарело после asr_lock — Whisper пропущен");
                return Ok(None);
            }
            return local_transcribe(ctx, s, dict, snippets, wav).map(Some);
        }
    }

    if !final_generation_is_current(ctx, expected_gen) {
        dbg_log("финал: поколение устарело до Whisper fallback — перезапуск пропущен");
        return Ok(None);
    }
    dbg_log(
        "финал: asr_lock занят >3 с — сбрасываем whisper-server и используем whisper-cli fallback",
    );
    restart_whisper_server(ctx, "final asr_lock timeout");
    local_transcribe_cli_only(ctx, s, dict, snippets, wav).map(Some)
}

fn local_transcribe_cli_only(
    ctx: &EngineCtx,
    s: &Settings,
    dict: &[postprocess::Dict],
    snippets: &[postprocess::Snippet],
    wav: &std::path::Path,
) -> anyhow::Result<String> {
    let model = resolve_model(s)?;
    let language = whisper_language_arg(&s.language);
    let base_prompt = whisper_base_prompt(&s.language);
    let prompt = postprocess::asr_bias_prompt(dict, snippets, base_prompt);
    transcribe_cli_with_runtime_fallback(
        ctx,
        &model,
        wav,
        &language,
        s.effective_threads(),
        prompt.as_deref(),
    )
}

fn whisper_runtime_dirs(ctx: &EngineCtx) -> Vec<std::path::PathBuf> {
    let primary = paths::whisper_dir(&ctx.app);
    let cpu = paths::whisper_cpu_dir(&ctx.app);
    if primary == cpu || ctx.whisper_accelerated_disabled.load(Ordering::Acquire) {
        vec![cpu]
    } else {
        vec![primary, cpu]
    }
}

fn is_accelerated_runtime(ctx: &EngineCtx, runtime: &std::path::Path) -> bool {
    runtime != paths::whisper_cpu_dir(&ctx.app)
}

fn disable_accelerated_runtime(ctx: &EngineCtx, runtime: &std::path::Path, error: &anyhow::Error) {
    if !is_accelerated_runtime(ctx, runtime) {
        return;
    }
    if !ctx
        .whisper_accelerated_disabled
        .swap(true, Ordering::AcqRel)
    {
        log::warn!(
            "accelerated whisper runtime {:?} disabled until restart: {error:#}",
            runtime
        );
        dbg_log("whisper: accelerated runtime disabled; using bundled CPU fallback");
    }
}

fn whisper_cli_timeout(wav: &std::path::Path, accelerated: bool) -> Duration {
    let audio_seconds = audio::wav_duration_secs_ceil(wav).unwrap_or(10);
    let (multiplier, maximum) = if accelerated { (3, 300) } else { (12, 1200) };
    Duration::from_secs(
        15u64
            .saturating_add(audio_seconds.saturating_mul(multiplier))
            .clamp(20, maximum),
    )
}

fn transcribe_cli_with_runtime_fallback(
    ctx: &EngineCtx,
    model: &std::path::Path,
    wav: &std::path::Path,
    language: &str,
    threads: u32,
    prompt: Option<&str>,
) -> anyhow::Result<String> {
    let runtimes = whisper_runtime_dirs(ctx);
    let mut last_error = None;
    for (index, runtime) in runtimes.iter().enumerate() {
        let params = AsrParams {
            whisper_dir: runtime,
            model_path: model,
            wav_path: wav,
            language,
            threads,
            initial_prompt: prompt,
        };
        let accelerated = is_accelerated_runtime(ctx, runtime);
        match asr::transcribe_cli_with_timeout(&params, whisper_cli_timeout(wav, accelerated)) {
            Ok(text) => {
                if index > 0 {
                    dbg_log("whisper: CUDA runtime failed, CPU CLI fallback succeeded");
                }
                return Ok(text);
            }
            Err(error) => {
                log::warn!("whisper-cli runtime {:?} failed: {error:#}", runtime);
                disable_accelerated_runtime(ctx, runtime, &error);
                last_error = Some(error);
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("whisper runtime не найден")))
}

fn local_transcribe(
    ctx: &EngineCtx,
    s: &Settings,
    dict: &[postprocess::Dict],
    snippets: &[postprocess::Snippet],
    wav: &std::path::Path,
) -> anyhow::Result<String> {
    let model = resolve_model(s)?;
    let language = whisper_language_arg(&s.language);
    // Короткая языковая затравка удерживает whisper в нужном режиме даже при
    // пустом словаре: ru — русский, auto/multi aliases — смешанная речь.
    let base_prompt = whisper_base_prompt(&s.language);
    let prompt = postprocess::asr_bias_prompt(dict, snippets, base_prompt);
    if s.engine == "whisper_cli" {
        transcribe_cli_with_runtime_fallback(
            ctx,
            &model,
            wav,
            &language,
            s.effective_threads(),
            prompt.as_deref(),
        )
    } else {
        let runtimes = whisper_runtime_dirs(ctx);
        for (index, runtime) in runtimes.iter().enumerate() {
            match ensure_server(ctx, runtime, &model, s.effective_threads())
                .and_then(|port| asr::transcribe_server(port, wav, &language, prompt.as_deref()))
            {
                Ok(text) => {
                    if index > 0 {
                        dbg_log("whisper: CUDA runtime failed, CPU server fallback succeeded");
                    }
                    return Ok(text);
                }
                Err(error) => {
                    log::warn!("whisper-server runtime {:?} failed: {error:#}", runtime);
                    disable_accelerated_runtime(ctx, runtime, &error);
                }
            }
        }
        log::warn!("whisper-server недоступен, откат на cli с CPU fallback");
        transcribe_cli_with_runtime_fallback(
            ctx,
            &model,
            wav,
            &language,
            s.effective_threads(),
            prompt.as_deref(),
        )
    }
}

fn load_corrections(conn: &Connection) -> Vec<postprocess::Correction> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT wrong, right FROM corrections ORDER BY hits DESC") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(postprocess::Correction {
                wrong: r.get(0)?,
                right: r.get(1)?,
            })
        }) {
            out.extend(rows.flatten());
        }
    }
    out
}

fn compact_instruction_source(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['“', '”', '«', '»'], "\"")
}

fn clamp_chars(value: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_chars {
            break;
        }
        out.push(ch);
    }
    out
}

fn one_line_instruction(value: &str) -> String {
    clamp_chars(&compact_instruction_source(value), 1800)
}

fn target_matches_memory(memory: &DictationMemory, actx: &crate::app_context::AppContext) -> bool {
    memory
        .target_fp
        .as_ref()
        .map(|target| {
            let same_window = if !target.window_id.is_empty() && !actx.window_id.is_empty() {
                target.exe == actx.exe && target.window_id == actx.window_id
            } else {
                target.exe == actx.exe && target.title == actx.title
            };
            same_window && !actx.field_id.is_empty() && target.field_id == actx.field_id
        })
        .unwrap_or(false)
}

fn dictation_memory_target(actx: &crate::app_context::AppContext) -> Option<DictationMemoryTarget> {
    if actx.field_id.trim().is_empty() {
        return None;
    }
    Some(DictationMemoryTarget {
        exe: actx.exe.clone(),
        title: actx.title.clone(),
        window_id: actx.window_id.clone(),
        field_id: actx.field_id.clone(),
    })
}

fn visible_previous_context_tail(
    actx: &crate::app_context::AppContext,
    field_before_dictation: &str,
    live_inserted: bool,
) -> Option<String> {
    if live_inserted {
        let previous = field_before_dictation.trim();
        if previous.is_empty() {
            None
        } else {
            Some(sentence_context_tail(field_before_dictation, 600))
        }
    } else {
        actx.focused_text_tail(600)
    }
}

fn last_dictation_context(
    ctx: &EngineCtx,
    actx: &crate::app_context::AppContext,
) -> Option<String> {
    let memory = ctx.dictation_memory.lock();
    if !target_matches_memory(&memory, actx) {
        return None;
    }
    memory.recent.back().map(dictation_memory_item_context)
}

fn dictation_memory_item_context(item: &DictationMemoryItem) -> String {
    let mut context = item.text.clone();
    if item.hard_boundary && !context.ends_with('\n') {
        context.push('\n');
    }
    context
}

fn previous_context_with_memory_boundary(
    ctx: &EngineCtx,
    actx: &crate::app_context::AppContext,
    visible: Option<String>,
) -> Option<String> {
    let memory = ctx.dictation_memory.lock();
    previous_context_with_memory_boundary_in(&memory, actx, visible)
}

fn previous_context_with_memory_boundary_in(
    memory: &DictationMemory,
    actx: &crate::app_context::AppContext,
    visible: Option<String>,
) -> Option<String> {
    if !target_matches_memory(memory, actx) {
        return visible;
    }
    let Some(last) = memory.recent.back() else {
        return visible;
    };
    match visible {
        Some(mut current) => {
            if last.hard_boundary
                && visible_tail_matches_memory_item(&current, &last.text, last.truncated_start)
                && !current.ends_with('\n')
            {
                current.push('\n');
            }
            Some(current)
        }
        None => Some(dictation_memory_item_context(last)),
    }
}

fn visible_tail_matches_memory_item(visible: &str, item: &str, truncated_start: bool) -> bool {
    let visible = compact_instruction_source(visible);
    let mut item = compact_instruction_source(item);
    if truncated_start {
        item = item.strip_prefix("...").unwrap_or(&item).to_string();
    }
    if item.is_empty() {
        return false;
    }
    if visible == item {
        return true;
    }
    let Some(prefix) = visible.strip_suffix(&item) else {
        return false;
    };
    // A complete token boundary prevents a short remembered fragment from
    // matching the ending of an unrelated word. Normal insertion before a
    // remembered phrase leaves exactly this whitespace boundary.
    truncated_start || prefix.chars().last().is_some_and(char::is_whitespace)
}

fn conversational_continuation_enabled(tone: &str) -> bool {
    matches!(tone, "" | "neutral" | "casual" | "very_casual" | "work")
}

fn should_preserve_unfinished_ending(
    settings: &Settings,
    tone: &str,
    is_exact_snippet: bool,
) -> bool {
    settings.auto_punct
        && !settings.verbatim
        && !is_exact_snippet
        && !matches!(tone, "code" | "verbatim")
}

fn continue_from_previous_context(text: &str, previous: Option<&str>, tone: &str) -> String {
    let Some(raw_prev) = previous.filter(|v| !v.trim().is_empty()) else {
        return text.to_string();
    };
    // Check the original tail before trimming it. A user-requested newline is
    // a hard boundary and must not disappear on the way into the continuation
    // detector.
    if previous_context_is_closed(raw_prev) {
        return text.to_string();
    }
    let prev = raw_prev.trim();
    let grammatically_open = postprocess::looks_unfinished_utterance(prev);
    if matches!(tone, "code" | "verbatim")
        || (!grammatically_open && !conversational_continuation_enabled(tone))
    {
        return text.to_string();
    }
    let next = if grammatically_open {
        // A high-confidence open clause may resume with any ordinary word, not
        // only a connective such as "и"/"потому что". The ASR capitalizes each
        // recording independently. Preserve it after a preposition because the
        // next word may be a proper name ("в" + "Москву").
        if postprocess::continuation_may_start_with_proper_name(prev, text) {
            text.trim_start().to_string()
        } else {
            lower_first_alphabetic(text.trim_start())
        }
    } else {
        let Some(next) = lower_if_continuation_start(text) else {
            return text.to_string();
        };
        next
    };
    if next
        .chars()
        .next()
        .map(|c| c.is_ascii_punctuation() || c.is_whitespace())
        .unwrap_or(false)
    {
        next
    } else {
        format!(" {next}")
    }
}

fn previous_context_is_closed(text: &str) -> bool {
    // A user-requested newline is an explicit boundary. Ignore only spaces and
    // tabs here so the newline is not erased before the check.
    if text.trim_end_matches([' ', '\t', '\r']).ends_with('\n') {
        return true;
    }
    let terminal = text
        .trim_end()
        .chars()
        .rev()
        .find(|c| !matches!(c, '"' | '\'' | ')' | ']' | '}' | '»' | '”'))
        .filter(|c| !c.is_whitespace());
    match terminal {
        Some('?' | '!') => true,
        // Ellipsis is an open pause. A single/full stop closes only a phrase
        // whose words are not themselves a high-confidence dangling clause.
        Some('.') => !postprocess::looks_unfinished_utterance(text),
        Some('…') => !postprocess::looks_unfinished_utterance(text),
        _ => false,
    }
}

fn compact_context_tail(value: &str, max_chars: usize) -> String {
    let compact = compact_instruction_source(value);
    let len = compact.chars().count();
    if len <= max_chars {
        return compact;
    }
    let mut tail: String = compact
        .chars()
        .rev()
        .take(max_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    tail = tail
        .trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, ',' | ';' | ':' | '-' | '—' | '–')
        })
        .to_string();
    format!("...{tail}")
}

fn sentence_context_tail(value: &str, max_chars: usize) -> String {
    let terminal_newline = value.trim_end_matches([' ', '\t', '\r']).ends_with('\n');
    let mut tail = compact_context_tail(value, max_chars);
    if terminal_newline && !tail.ends_with('\n') {
        tail.push('\n');
    }
    tail
}

fn merge_context_summary(current: &str, old: &str) -> String {
    let merged = if current.trim().is_empty() {
        old.to_string()
    } else {
        format!("{current} {old}")
    };
    compact_context_tail(&merged, DICTATION_CONTEXT_SUMMARY_CHARS)
}

fn recent_context_len(recent: &VecDeque<DictationMemoryItem>) -> usize {
    recent.iter().map(|v| v.text.chars().count()).sum()
}

fn remember_dictation_context(
    ctx: &EngineCtx,
    actx: &crate::app_context::AppContext,
    text: &str,
    explicit_terminal: bool,
) {
    let mut memory = ctx.dictation_memory.lock();
    remember_dictation_context_in(&mut memory, actx, text, explicit_terminal);
}

fn remember_dictation_context_in(
    memory: &mut DictationMemory,
    actx: &crate::app_context::AppContext,
    text: &str,
    explicit_terminal: bool,
) {
    let compact = sentence_context_tail(text, DICTATION_CONTEXT_ITEM_CHARS);
    if compact.is_empty() {
        return;
    }

    let Some(target_fp) = dictation_memory_target(actx) else {
        // Without a stable field identifier, reusing hidden memory in another
        // empty control of the same window can leak sentence context. The
        // visible focused-field tail remains the safe continuation source.
        memory.target_fp = None;
        memory.summary.clear();
        memory.recent.clear();
        return;
    };
    if memory.target_fp.as_ref() != Some(&target_fp) {
        memory.target_fp = Some(target_fp);
        memory.summary.clear();
        memory.recent.clear();
    }

    let hard_boundary = compact.ends_with('\n') || explicit_terminal;
    let truncated_start =
        compact_instruction_source(text).chars().count() > DICTATION_CONTEXT_ITEM_CHARS;
    memory.recent.push_back(DictationMemoryItem {
        text: compact,
        hard_boundary,
        truncated_start,
    });
    while memory.recent.len() > DICTATION_CONTEXT_RECENT_LIMIT
        || recent_context_len(&memory.recent) > DICTATION_CONTEXT_RECENT_CHARS
    {
        let Some(old) = memory.recent.pop_front() else {
            break;
        };
        memory.summary = merge_context_summary(&memory.summary, &old.text);
    }
}

fn rewrite_context_hint(
    ctx: &EngineCtx,
    actx: &crate::app_context::AppContext,
    current_document: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(doc) = current_document.map(str::trim).filter(|v| !v.is_empty()) {
        parts.push(format!(
            "Готовый/выделенный текст для правки: {}",
            compact_context_tail(doc, 1200)
        ));
    }

    if current_document.is_none() {
        if let Some(field_tail) = actx.focused_text_tail(600) {
            parts.push(format!("Хвост текста в активном поле: {field_tail}"));
        }
    }

    let memory = ctx.dictation_memory.lock();
    if target_matches_memory(&memory, actx) {
        if !memory.summary.trim().is_empty() {
            parts.push(format!(
                "Краткая выжимка более ранней диктовки: {}",
                memory.summary
            ));
        }
        if !memory.recent.is_empty() {
            parts.push(format!(
                "Последние вставленные фрагменты: {}",
                memory
                    .recent
                    .iter()
                    .map(|v| v.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ));
        }
    }

    if parts.is_empty() {
        None
    } else {
        Some(clamp_chars(&parts.join("\n"), 1800))
    }
}

fn push_unique_prompt_item(
    items: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: &str,
    max_chars: usize,
) {
    let cleaned = compact_instruction_source(value);
    let cleaned = cleaned
        .trim_matches(|c: char| matches!(c, ',' | ';' | ':' | '.' | '…'))
        .trim();
    if cleaned.is_empty() {
        return;
    }

    let item = clamp_chars(cleaned, max_chars);
    let key = item.to_lowercase();
    if seen.insert(key) {
        items.push(item);
    }
}

fn build_asr_prompt(
    actx: &crate::app_context::AppContext,
    tone: &str,
    previous_context_tail: Option<&str>,
    dict: &[postprocess::Dict],
    snippets: &[postprocess::Snippet],
    corrections: &[postprocess::Correction],
) -> Option<String> {
    let mut parts = vec![
        "Speech recognition context only: transcribe what was said, preserve Russian/English/other language switches, do not rewrite. Recording stop is not necessarily a sentence boundary: do not add a final period when the utterance is grammatically unfinished.".to_string(),
    ];

    let app = app_label_for_payload(actx);
    if !app.trim().is_empty() {
        if tone.trim().is_empty() || tone == "neutral" {
            parts.push(format!("Active app: {app}."));
        } else {
            parts.push(format!("Active app: {app}; style context: {tone}."));
        }
    }

    if let Some(raw_previous) = previous_context_tail.filter(|value| !value.trim().is_empty()) {
        let terminal_newline = raw_previous
            .trim_end_matches([' ', '\t', '\r'])
            .ends_with('\n');
        let previous = raw_previous.trim();
        let tail = compact_context_tail(previous, ASR_PROMPT_PREVIOUS_CHARS);
        if terminal_newline {
            parts.push(format!(
                "Previous same-field text tail (explicit line boundary; do not merge across it): {tail}"
            ));
        } else if postprocess::looks_unfinished_utterance(previous) {
            parts.push(format!(
                "Previous same-field text tail (grammatically unfinished; current speech may continue it): {tail}"
            ));
        } else {
            parts.push(format!("Previous same-field text tail: {tail}."));
        }
    }

    let mut terms = Vec::new();
    let mut seen_terms = HashSet::new();
    for term in BUILTIN_ASR_TERMS {
        push_unique_prompt_item(&mut terms, &mut seen_terms, term, 80);
    }
    for d in dict {
        if terms.len() >= ASR_PROMPT_TERM_LIMIT {
            break;
        }
        push_unique_prompt_item(&mut terms, &mut seen_terms, &d.replacement, 80);
        if terms.len() >= ASR_PROMPT_TERM_LIMIT {
            break;
        }
        push_unique_prompt_item(&mut terms, &mut seen_terms, &d.term, 80);
    }
    if !terms.is_empty() {
        parts.push(format!(
            "Likely names and technical terms: {}.",
            terms.join(", ")
        ));
    }

    let mut snippet_triggers = Vec::new();
    let mut seen_snippets = HashSet::new();
    for sn in snippets.iter().take(ASR_PROMPT_SNIPPET_LIMIT) {
        push_unique_prompt_item(&mut snippet_triggers, &mut seen_snippets, &sn.trigger, 80);
    }
    if !snippet_triggers.is_empty() {
        parts.push(format!(
            "Possible spoken snippet triggers: {}.",
            snippet_triggers.join(", ")
        ));
    }

    let mut correction_pairs = Vec::new();
    let mut seen_corrections = HashSet::new();
    for c in corrections.iter().take(ASR_PROMPT_CORRECTION_LIMIT) {
        let wrong = compact_instruction_source(&c.wrong);
        let right = compact_instruction_source(&c.right);
        let wrong = wrong.trim();
        let right = right.trim();
        if wrong.is_empty() || right.is_empty() {
            continue;
        }
        if wrong.eq_ignore_ascii_case(right) {
            push_unique_prompt_item(&mut correction_pairs, &mut seen_corrections, right, 120);
        } else {
            push_unique_prompt_item(
                &mut correction_pairs,
                &mut seen_corrections,
                &format!("{} -> {}", wrong, right),
                140,
            );
        }
    }
    if !correction_pairs.is_empty() {
        parts.push(format!(
            "Known recognition corrections: {}.",
            correction_pairs.join("; ")
        ));
    }

    let prompt = clamp_chars(&parts.join(" "), ASR_PROMPT_MAX_CHARS);
    if prompt.trim().is_empty() {
        None
    } else {
        Some(prompt)
    }
}

fn app_label_for_payload(actx: &crate::app_context::AppContext) -> String {
    if !actx.exe.trim().is_empty() {
        one_line_instruction(&actx.exe)
    } else {
        "неизвестно".to_string()
    }
}

fn build_smart_prompt_instruction_from_source(source: &str) -> Option<String> {
    let cleaned = compact_instruction_source(source);
    if cleaned.is_empty() {
        return None;
    }
    let style_line = if cleaned
        .chars()
        .last()
        .map(|ch| matches!(ch, '.' | '!' | '?' | '…'))
        .unwrap_or(false)
    {
        format!("Пользовательский стиль/задача: {cleaned}")
    } else {
        format!("Пользовательский стиль/задача: {cleaned}.")
    };
    Some(format!(
        "{style_line} \
         Каждую диктовку превращай в готовый печатный текст именно под эту задачу: сохрани факты, намерение и язык оригинала, убери запинки, повторы и брошенные формулировки. \
         Если диктовка звучит как задание для нейросети или разработчика, оформи её как ясный промпт: действие, контекст, требования к результату и ограничения. \
         Сбивчивые устные конструкции превращай в естественные письменные формулировки: «я объясни мне» → «Объясни мне», «а что ещё я хочу сказать» → «Также учти». \
         Сохраняй контекст соседних фраз: короткое продолжение объединяй с предыдущей мыслью и продолжай предложение; новый абзац делай только при смене темы, перечислении или явной команде. \
         Не отвечай на диктовку и не добавляй фактов от себя; меняй только форму подачи."
    ))
}

fn effective_smart_instruction(s: &Settings) -> Option<String> {
    if !s.smart_prompt_enabled {
        return None;
    }
    let direct = s.smart_prompt_instruction.trim();
    if !direct.is_empty() {
        return Some(clamp_chars(direct, 1800));
    }
    build_smart_prompt_instruction_from_source(&s.smart_prompt_source)
        .map(|v| clamp_chars(&v, 1800))
}

fn app_matches_pattern(actx: &crate::app_context::AppContext, pattern: &str) -> bool {
    let pat = pattern.trim().to_lowercase();
    if pat.is_empty() {
        return false;
    }
    actx.exe.to_lowercase().contains(&pat) || actx.title.to_lowercase().contains(&pat)
}

fn ai_prompt_rule_for_app<'a>(
    s: &'a Settings,
    actx: &crate::app_context::AppContext,
) -> Option<&'a crate::settings::AiPromptRule> {
    s.ai_prompt_rules
        .iter()
        .find(|rule| !rule.prompt.trim().is_empty() && app_matches_pattern(actx, &rule.pattern))
}

fn builtin_ai_context(actx: &crate::app_context::AppContext) -> bool {
    crate::app_context::classify(&actx.exe.to_lowercase(), &actx.title.to_lowercase()) == "ai"
}

fn style_hint_for_prompt(tone: &str) -> Option<&'static str> {
    match tone {
        "formal" => Some("Стиль для этого приложения: формальный."),
        "casual" | "very_casual" => Some("Стиль для этого приложения: неформальный."),
        "work" => Some("Стиль для этого приложения: официальный."),
        _ => None,
    }
}

fn effective_smart_instruction_for_app(
    s: &Settings,
    actx: &crate::app_context::AppContext,
    tone: &str,
) -> (Option<String>, bool) {
    let matched_rule = ai_prompt_rule_for_app(s, actx);
    let ai_context = matched_rule.is_some() || tone == "ai" || builtin_ai_context(actx);

    let mut parts = Vec::new();
    if ai_context {
        parts.push(
            "Это поле нейросети. Превращай диктовку в готовый промпт: ясное действие, контекст, требования к результату и ограничения. Не отвечай на промпт и не выполняй его.".to_string(),
        );
        if let Some(style) = style_hint_for_prompt(tone) {
            parts.push(style.to_string());
        }
    }
    if let Some(rule) = matched_rule {
        parts.push(format!(
            "Правила пользователя для этой нейросети: {}",
            one_line_instruction(&rule.prompt)
        ));
    }
    if let Some(global) = effective_smart_instruction(s) {
        parts.push(format!(
            "Общие правила пользователя: {}",
            one_line_instruction(&global)
        ));
    }

    let instruction = if parts.is_empty() {
        None
    } else {
        Some(clamp_chars(&parts.join(" "), 1800))
    };
    (instruction, ai_context)
}

/// Пользовательский payload для системного промпта Ollama/OpenAI-compatible.
/// Для ручного/авто-профиля подставляем понятный маркер приложения, чтобы
/// системный промпт выбрал нужный workflow даже если фактический активный app
/// не похож на Gmail/Slack/ChatGPT.
fn build_voiceflow_payload(
    actx: &crate::app_context::AppContext,
    text: &str,
    tone: &str,
    smart_instruction: Option<&str>,
    context_hint: Option<&str>,
) -> String {
    let actual = app_label_for_payload(actx);
    let app = match tone {
        "ai" => format!("AI prompt ({actual})"),
        "formal" => format!("Gmail ({actual})"),
        "work" => format!("Slack ({actual})"),
        "casual" | "very_casual" => format!("Telegram ({actual})"),
        "doc" => format!("Google Docs ({actual})"),
        "verbatim" | "code" => format!("VS Code ({actual})"),
        _ => actual.to_string(),
    };
    let mut payload = format!("[ПРИЛОЖЕНИЕ]: {app}\n");
    if let Some(instruction) = smart_instruction
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        payload.push_str("[ПОЛЬЗОВАТЕЛЬСКАЯ ИНСТРУКЦИЯ]: ");
        payload.push_str(&one_line_instruction(instruction));
        payload.push('\n');
    }
    if let Some(context) = context_hint.map(str::trim).filter(|v| !v.is_empty()) {
        payload.push_str("[ОКРУЖЕНИЕ]: ");
        payload.push_str(&one_line_instruction(context));
        payload.push('\n');
        payload.push_str("[КАК ИСПОЛЬЗОВАТЬ ОКРУЖЕНИЕ]: ");
        payload.push_str(
            "используй его только для пунктуации, продолжения предложения, местоимений и стиля; не добавляй факты, которых нет в диктовке.",
        );
        payload.push('\n');
    }
    payload.push_str("[ДИКТОВКА]: ");
    payload.push_str(text);
    payload
}

struct RewriteRequest<'a> {
    actx: &'a crate::app_context::AppContext,
    text: &'a str,
    tone: &'a str,
    smart_instruction: Option<&'a str>,
    context_hint: Option<&'a str>,
    corrections: &'a [postprocess::Correction],
    force: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RewriteBackendRoute {
    Gemini,
    OpenAiCompat,
    Ollama,
}

/// Рефайн идёт только в явно выбранный бэкенд. До 2.0.1 любой
/// недоступный Gemini/OpenAI-compatible незаметно падал в Ollama,
/// потому что её дефолтный localhost URL считался "configured". Это
/// запускало Qwen3 на CPU без отдельного opt-in.
fn configured_rewrite_backend(s: &Settings) -> Option<RewriteBackendRoute> {
    match s.ai_backend.as_str() {
        "gemini" if crate::gemini::available(&s.ai_api_key) => Some(RewriteBackendRoute::Gemini),
        "openai_compat" if crate::rewrite::configured(s) => Some(RewriteBackendRoute::OpenAiCompat),
        "ollama" if crate::ollama::configured(&s.ollama_url) => Some(RewriteBackendRoute::Ollama),
        _ => None,
    }
}

/// Automatic rewrite may improve form, but it must remain anchored in what the
/// user actually dictated. Only explicit Improve Selection (`force`) may bypass
/// this guard because that command intentionally requests a transformation.
fn rewrite_is_grounded(input: &str, output: &str) -> bool {
    let input_chars = input.chars().count();
    let output_chars = output.chars().count();
    if output_chars > input_chars.saturating_mul(2).saturating_add(32) {
        return false;
    }

    let tokens = |value: &str| {
        value
            .split(|c: char| !c.is_alphanumeric())
            .filter(|part| !part.is_empty())
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    let input_tokens = tokens(input);
    let output_tokens = tokens(output);
    if input_tokens.is_empty() {
        return output_tokens.is_empty();
    }
    if output_tokens.len() > input_tokens.len().saturating_add(4) {
        return false;
    }

    // Automatic refinement is allowed to change punctuation/casing and add a
    // tiny whitelist of harmless fillers, but never to change content order,
    // morphology or logical glue. Even noun case can reverse subject/object,
    // so automatic mode deliberately fails closed on every content-token edit.
    // Prepositions and conjunctions are meaning-bearing ("в" != "из", "и" !=
    // "или"), so only articles and polite fillers may appear/disappear.
    let mut input_content = input_tokens
        .iter()
        .filter(|token| !rewrite_structural_token(token))
        .collect::<Vec<_>>();
    if input_content.is_empty() {
        input_content = input_tokens.iter().collect();
    }
    let mut output_content = output_tokens
        .iter()
        .filter(|token| !rewrite_structural_token(token))
        .collect::<Vec<_>>();
    if output_content.is_empty() {
        output_content = output_tokens.iter().collect();
    }

    input_content.len() == output_content.len()
        && input_content
            .iter()
            .zip(output_content.iter())
            .all(|(source, candidate)| source.as_str() == candidate.as_str())
}

fn rewrite_structural_token(token: &str) -> bool {
    matches!(token, "пожалуйста" | "please" | "a" | "an" | "the")
}

fn refine_text_with_fallback(s: &Settings, request: RewriteRequest<'_>) -> (String, bool) {
    let RewriteRequest {
        actx,
        text,
        tone,
        smart_instruction,
        context_hint,
        corrections,
        force,
    } = request;

    if text.trim().is_empty() {
        return (String::new(), false);
    }
    if s.verbatim && !force {
        return (text.to_string(), false);
    }
    let has_smart_instruction = smart_instruction
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    let target_tone =
        if tone.is_empty() || tone == "neutral" || tone == "verbatim" || tone == "code" {
            if force || has_smart_instruction {
                "doc"
            } else {
                tone
            }
        } else {
            tone
        };
    if !force && !has_smart_instruction && (target_tone.is_empty() || target_tone == "neutral") {
        return (text.to_string(), false);
    }

    let mut attempts: Vec<Box<dyn Fn() -> anyhow::Result<String>>> = Vec::with_capacity(1);
    match configured_rewrite_backend(s) {
        Some(RewriteBackendRoute::Gemini) => {
            let key = s.ai_api_key.clone();
            let model = s.ai_model.clone();
            let instruction =
                build_tone_instruction(target_tone, smart_instruction, context_hint, corrections);
            let input = text.to_string();
            attempts.push(Box::new(move || {
                crate::gemini::refine(&key, &model, &instruction, &input)
            }));
        }
        Some(RewriteBackendRoute::OpenAiCompat) => {
            let settings = s.clone();
            let user =
                build_voiceflow_payload(actx, text, target_tone, smart_instruction, context_hint);
            attempts.push(Box::new(move || {
                crate::rewrite::refine(&settings, crate::ollama::SYSTEM_PROMPT, &user)
            }));
        }
        Some(RewriteBackendRoute::Ollama) => {
            let url = s.ollama_url.clone();
            let model = s.ollama_model.clone();
            let user =
                build_voiceflow_payload(actx, text, target_tone, smart_instruction, context_hint);
            attempts.push(Box::new(move || {
                crate::ollama::refine(&url, &model, crate::ollama::SYSTEM_PROMPT, &user)
            }));
        }
        None => {}
    }

    for attempt in attempts {
        match attempt() {
            Ok(r) if !r.trim().is_empty() => {
                let refined = postprocess::normalize_spaces(r.trim());
                if force || rewrite_is_grounded(text, &refined) {
                    return (refined, true);
                }
                log::warn!(
                    "рерайт отклонён защитой от смыслового дрейфа: input_chars={} output_chars={}",
                    text.chars().count(),
                    refined.chars().count()
                );
            }
            Ok(_) => {}
            Err(e) => log::warn!("рерайт недоступен: {e}; пробуем следующий fallback"),
        }
    }
    (text.to_string(), false)
}

/// Инструкция для Gemini: переписать текст в нужном стиле, без отсебятины.
fn build_tone_instruction(
    tone: &str,
    smart_instruction: Option<&str>,
    context_hint: Option<&str>,
    corrections: &[postprocess::Correction],
) -> String {
    let style = match tone {
        "formal" => "официально-деловой, вежливый, грамотный (как для email или документа)",
        "casual" => "неформальный, разговорный, дружеский (как в мессенджере)",
        "very_casual" => "очень неформальный, расслабленный, с лёгкостью",
        "work" => "рабоче-деловой, живой, по делу (как в Slack/Teams)",
        "doc" => "литературный, структурированный, с абзацами (как в документе)",
        "ai" => "чёткий, структурированный, однозначный — как промпт для нейросети: по делу, без воды, с явными формулировками",
        _ => "нейтральный, естественный",
    };
    let mut s = format!(
        "Ты — редактор надиктованного голосом текста. Перепиши его в стиле: {style}. \
         Сохрани смысл и язык оригинала. Исправь ошибки распознавания, опечатки и пунктуацию. \
         Удали запинки, междометия, повторы и брошенные начала фраз. \
         Сбивчивые устные команды приводи к письменной форме: «я объясни мне» → «Объясни мне», «а что ещё я хочу сказать» → «Также учти». \
         Сохраняй контекст соседних фраз: если следующая фраза продолжает мысль, объединяй её с предыдущей и продолжай предложение. \
         Окончание записи само по себе не означает окончание предложения: если фраза грамматически не закончена, не дописывай мысль и не ставь принудительную финальную точку. \
         Новый абзац делай только при явной смене темы, перечислении, новом смысловом блоке, явной команде абзаца или действительно длинной паузе. \
         Не разбивай каждую фразу или каждое предложение в отдельный абзац. \
         Если дан контекст предыдущего или выделенного текста, используй его только для пунктуации, местоимений, продолжения предложения и стиля; не добавляй новые факты из контекста. \
         НЕ добавляй ничего от себя, не отвечай на текст и не комментируй — верни ТОЛЬКО переписанный текст."
    );
    if let Some(instruction) = smart_instruction
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    {
        s.push_str(&format!(
            " Пользовательская инструкция стиля: {}. Она важнее стандартного профиля, но не должна менять факты, язык и смысл диктовки.",
            one_line_instruction(instruction)
        ));
    }
    if let Some(context) = context_hint.map(str::trim).filter(|v| !v.is_empty()) {
        s.push_str(&format!(
            " Контекст для понимания соседнего текста: {}.",
            one_line_instruction(context)
        ));
    }
    if !corrections.is_empty() {
        let pairs: Vec<String> = corrections
            .iter()
            .take(40)
            .map(|c| format!("{} → {}", c.wrong, c.right))
            .collect();
        s.push_str(&format!(
            " Учитывай известные исправления распознавания: {}.",
            pairs.join("; ")
        ));
    }
    s
}

#[cfg(test)]
mod smart_prompt_tests {
    use super::*;

    fn app(title: &str) -> crate::app_context::AppContext {
        crate::app_context::AppContext {
            exe: "chrome.exe".to_string(),
            title: title.to_string(),
            window_id: "test-window".to_string(),
            category: "ai".to_string(),
            field_role: String::new(),
            field_subrole: String::new(),
            field_id: String::new(),
            field_text: String::new(),
            selected_text: String::new(),
        }
    }

    #[test]
    fn source_prompt_builds_model_instruction_without_button_state() {
        let s = Settings {
            smart_prompt_enabled: true,
            smart_prompt_source: "Я хочу, чтобы это звучало как печатный текст.".to_string(),
            smart_prompt_instruction: String::new(),
            ..Settings::default()
        };

        let instruction = effective_smart_instruction(&s).expect("instruction from source");

        assert!(instruction.contains("готовый печатный текст"));
        assert!(instruction.contains("я объясни мне"));
        assert!(instruction.contains("Не отвечай на диктовку"));
        assert!(!instruction.contains("текст.."));
    }

    #[test]
    fn remote_rewrite_guard_ignores_loopback_but_covers_cloud_backends() {
        let local = Settings::default();
        assert!(!potentially_remote_rewrite(&local));

        let gemini = Settings {
            ai_backend: "gemini".into(),
            ai_api_key: "configured-for-test".into(),
            ..Settings::default()
        };
        assert!(potentially_remote_rewrite(&gemini));

        let remote_compat = Settings {
            ai_backend: "openai_compat".into(),
            rewrite_base_url: "https://api.example.test/v1".into(),
            rewrite_model: "example-model".into(),
            ..Settings::default()
        };
        assert!(potentially_remote_rewrite(&remote_compat));

        let local_compat = Settings {
            ai_backend: "openai_compat".into(),
            rewrite_base_url: "http://127.0.0.1:11434/v1".into(),
            rewrite_model: "local-model".into(),
            ..Settings::default()
        };
        assert!(!potentially_remote_rewrite(&local_compat));
    }

    #[test]
    fn voiceflow_payload_includes_saved_prompt_for_neural_rewrite() {
        let prompt = "Делай из диктовки промпт для Codex и явно упоминай Computer Use.";
        let payload = build_voiceflow_payload(
            &app("ChatGPT"),
            "я объясни мне по поводу архитектуры проекта а ещё используй компьютер юз",
            "ai",
            Some(prompt),
            None,
        );

        assert!(payload.contains("[ПРИЛОЖЕНИЕ]: AI prompt (chrome.exe)"));
        assert!(!payload.contains("ChatGPT"));
        assert!(payload.contains("[ПОЛЬЗОВАТЕЛЬСКАЯ ИНСТРУКЦИЯ]:"));
        assert!(payload.contains("Делай из диктовки промпт для Codex"));
        assert!(payload.contains("[ДИКТОВКА]: я объясни мне"));
    }

    #[test]
    fn gemini_instruction_keeps_user_prompt_above_base_style() {
        let instruction = build_tone_instruction(
            "ai",
            Some("Преобразуй фразу в структурный промпт для нейросети"),
            None,
            &[],
        );

        assert!(instruction.contains("Пользовательская инструкция стиля"));
        assert!(instruction.contains("структурный промпт"));
        assert!(instruction.contains("Она важнее стандартного профиля"));
    }

    #[test]
    fn voiceflow_payload_includes_context_as_environment_not_dictation() {
        let payload = build_voiceflow_payload(
            &app("Telegram"),
            "наверное задержусь на работе",
            "casual",
            None,
            Some("Последние вставленные фрагменты: Я сегодня"),
        );

        assert!(payload.contains("[ОКРУЖЕНИЕ]:"));
        assert!(payload.contains("Последние вставленные фрагменты"));
        assert!(payload.contains("[КАК ИСПОЛЬЗОВАТЬ ОКРУЖЕНИЕ]:"));
        assert!(payload.contains("[ДИКТОВКА]: наверное задержусь"));
    }

    #[test]
    fn automatic_rewrite_grounding_rejects_phantom_content() {
        assert!(rewrite_is_grounded(
            "Отправь отчёт клиенту завтра утром",
            "Отправь отчёт клиенту завтра утром."
        ));
        assert!(rewrite_is_grounded(
            "Отправь отчёт",
            "Пожалуйста, отправь отчёт."
        ));
        assert!(!rewrite_is_grounded(
            "Отправь отчёт клиенту завтра утром",
            "Сегодня прекрасная погода, поэтому предлагаю обсудить новую стратегию продаж."
        ));
        assert!(!rewrite_is_grounded(
            "Привет",
            "Конечно, вот подробный ответ с фактами, которых пользователь не произносил"
        ));
        assert!(!rewrite_is_grounded("Привет", "Сегодня отличная погода"));
        assert!(!rewrite_is_grounded(
            "удали файл",
            "удали важный файл и перезагрузи сервер"
        ));
        for (input, output) in [
            ("скопируй файл в архив", "скопируй файл из архива"),
            ("удали файл и папку", "удали файл или папку"),
            ("отправь файл", "отправь без файла"),
            ("удали файл", "удари файл"),
            ("открой порт", "открой торт"),
            ("кот ест мышь", "мышь ест кот"),
            ("Алиса любит Бориса", "Борис любит Алису"),
            ("Алиса любит Бориса", "Алису любит Борис"),
        ] {
            assert!(
                !rewrite_is_grounded(input, output),
                "logic-changing rewrite passed: {input:?} -> {output:?}"
            );
        }
        assert!(rewrite_is_grounded("Привет", "Привет!"));
    }

    #[test]
    fn gemini_instruction_includes_context_guardrail() {
        let instruction = build_tone_instruction(
            "doc",
            None,
            Some("Краткая выжимка более ранней диктовки: обсуждали релиз"),
            &[],
        );

        assert!(instruction.contains("Контекст для понимания соседнего текста"));
        assert!(instruction.contains("не добавляй новые факты из контекста"));
        assert!(instruction.contains("Окончание записи само по себе"));
    }

    #[test]
    fn context_tail_is_clamped_from_the_end() {
        let value = "раз два три четыре пять шесть";
        let clipped = compact_context_tail(value, 12);

        assert!(clipped.starts_with("..."));
        assert!(clipped.contains("пять шесть"));
        assert!(clipped.chars().count() <= 15);
    }

    #[test]
    fn ai_prompt_rule_matches_app_even_when_visible_style_is_formal() {
        let s = Settings {
            smart_prompt_enabled: false,
            ai_prompt_rules: vec![crate::settings::AiPromptRule {
                pattern: "chatgpt".to_string(),
                prompt: "Всегда оформляй как промпт для GPT с чеклистом результата.".to_string(),
            }],
            ..Settings::default()
        };
        let actx = app("ChatGPT");

        let (instruction, is_ai) = effective_smart_instruction_for_app(&s, &actx, "formal");
        let instruction = instruction.expect("per-network instruction");

        assert!(is_ai);
        assert!(instruction.contains("поле нейросети"));
        assert!(instruction.contains("Стиль для этого приложения: формальный"));
        assert!(instruction.contains("чеклистом результата"));
    }

    #[test]
    fn builtin_codex_context_forces_ai_prompt_without_saved_rule() {
        let s = Settings {
            smart_prompt_enabled: false,
            ai_prompt_rules: Vec::new(),
            ..Settings::default()
        };
        let actx = crate::app_context::AppContext {
            exe: "codex.exe".to_string(),
            title: "Codex".to_string(),
            window_id: "test-window".to_string(),
            category: "ai".to_string(),
            field_role: String::new(),
            field_subrole: String::new(),
            field_id: String::new(),
            field_text: String::new(),
            selected_text: String::new(),
        };

        let (instruction, is_ai) = effective_smart_instruction_for_app(&s, &actx, "work");

        assert!(is_ai);
        assert!(instruction
            .expect("ai default instruction")
            .contains("готовый промпт"));
    }

    #[test]
    fn builtin_ai_context_does_not_block_final_insert_on_rewrite() {
        let s = Settings {
            smart_prompt_enabled: false,
            ai_prompt_rules: Vec::new(),
            ..Settings::default()
        };
        let actx = app("Claude");

        let (instruction, is_ai) = effective_smart_instruction_for_app(&s, &actx, "ai");

        assert!(is_ai);
        assert!(
            instruction.is_some(),
            "ASR/context still gets prompt shaping"
        );
        assert!(
            !final_rewrite_eligible(&s, "ai", instruction.is_some(), false),
            "built-in AI profile must keep insertion fast"
        );
    }

    #[test]
    fn clean_defaults_never_schedule_synchronous_rewrite() {
        let s = Settings::default();

        assert_eq!(s.ai_backend, "off");
        assert_eq!(configured_rewrite_backend(&s), None);
        for tone in ["ai", "casual", "very_casual", "work", "formal", "doc"] {
            assert!(
                !final_rewrite_eligible(&s, tone, true, true),
                "clean install must not schedule a blocking LLM for {tone}"
            );
        }
    }

    #[test]
    fn selected_backend_never_falls_through_to_implicit_ollama() {
        let unavailable_gemini = Settings {
            ai_backend: "gemini".into(),
            ai_api_key: String::new(),
            ollama_url: "http://localhost:11434".into(),
            ..Settings::default()
        };
        assert_eq!(configured_rewrite_backend(&unavailable_gemini), None);

        let configured_gemini = Settings {
            ai_api_key: "configured-for-test".into(),
            ..unavailable_gemini
        };
        assert_eq!(
            configured_rewrite_backend(&configured_gemini),
            Some(RewriteBackendRoute::Gemini)
        );

        let explicit_ollama = Settings {
            ai_backend: "ollama".into(),
            ollama_url: "http://localhost:11434".into(),
            ..Settings::default()
        };
        assert_eq!(
            configured_rewrite_backend(&explicit_ollama),
            Some(RewriteBackendRoute::Ollama)
        );
    }

    #[test]
    fn explicit_ai_rule_with_backend_opts_into_sync_rewrite() {
        let s = Settings {
            ai_backend: "ollama".into(),
            smart_prompt_enabled: false,
            ai_prompt_rules: vec![crate::settings::AiPromptRule {
                pattern: "claude".to_string(),
                prompt: "Делай структурный промпт с критериями готовности.".to_string(),
            }],
            ..Settings::default()
        };
        let actx = app("Claude");

        let (instruction, is_ai) = effective_smart_instruction_for_app(&s, &actx, "ai");

        assert!(is_ai);
        assert!(instruction.is_some());
        assert!(final_rewrite_eligible(
            &s,
            "ai",
            instruction.is_some(),
            true
        ));
    }

    #[test]
    fn asr_prompt_includes_bias_sources_without_snippet_body() {
        let dict = vec![postprocess::Dict {
            term: "виспр флоу".to_string(),
            replacement: "Wispr Flow".to_string(),
        }];
        let snippets = vec![postprocess::Snippet {
            trigger: "sig".to_string(),
            content: "super secret expanded template".to_string(),
            is_template: true,
        }];
        let corrections = vec![postprocess::Correction {
            wrong: "Виспа Фолл".to_string(),
            right: "Wispr Flow".to_string(),
        }];

        let prompt = build_asr_prompt(
            &app("Codex"),
            "ai",
            Some("предыдущий хвост про Tauri и whisper.cpp"),
            &dict,
            &snippets,
            &corrections,
        )
        .expect("prompt");

        assert!(prompt.contains("preserve Russian/English"));
        assert!(prompt.contains("Recording stop is not necessarily"));
        assert!(prompt.contains("Previous same-field text tail"));
        assert!(prompt.contains("Wispr Flow"));
        assert!(prompt.contains("VoxFlow"));
        assert!(prompt.contains("sig"));
        assert!(prompt.contains("Виспа Фолл -> Wispr Flow"));
        assert!(!prompt.contains("super secret expanded template"));
        assert!(prompt.chars().count() <= ASR_PROMPT_MAX_CHARS);

        let unfinished =
            build_asr_prompt(&app("Codex"), "ai", Some("Я хочу, чтобы ты"), &[], &[], &[])
                .expect("unfinished prompt");
        assert!(unfinished.contains("grammatically unfinished"));
        assert!(!unfinished.contains("Я хочу, чтобы ты."));

        let newline_boundary = build_asr_prompt(
            &app("Codex"),
            "ai",
            Some("Я хочу, чтобы ты\n"),
            &[],
            &[],
            &[],
        )
        .expect("newline boundary prompt");
        assert!(newline_boundary.contains("explicit line boundary"));
        assert!(
            !newline_boundary.contains("Previous same-field text tail (grammatically unfinished;")
        );
    }
}

/// Close the privacy-scoped learning window and forget its in-memory field
/// snapshot. No typed text is retained beyond this point.
fn close_correction_capture(ctx: &EngineCtx) {
    ctx.correction_capture_active
        .store(false, Ordering::Release);
    *ctx.last_inject.lock() = None;
}

/// The keyboard hook deliberately sends no key or character. It only marks the
/// last injected field dirty; Accessibility/UIA supplies the resulting value
/// after the user stops typing for a short debounce interval.
fn note_manual_edit(ctx: &EngineCtx) {
    if !ctx.correction_capture_active.load(Ordering::Acquire) {
        return;
    }
    let now = Instant::now();
    let mut slot = ctx.last_inject.lock();
    let Some(last) = slot.as_mut() else {
        ctx.correction_capture_active
            .store(false, Ordering::Release);
        return;
    };
    if now.saturating_duration_since(last.at) > TYPED_CORRECTION_WINDOW {
        *slot = None;
        ctx.correction_capture_active
            .store(false, Ordering::Release);
        return;
    }
    last.mark_manual_edit(now);
}

enum TypedLearningPoll {
    Wait,
    Close,
    Learn(Vec<(String, String)>),
}

enum TypedLearningDecision {
    Wait,
    Stop,
    Persist(Vec<(String, String)>),
}

/// Called while `last_inject` is locked. Consuming the slot and disabling the
/// keyboard signal in this same critical section prevents a new ManualEdit
/// epoch from slipping between a Learn decision and a later cleanup lock.
fn consume_typed_learning_poll_locked(
    slot: &mut Option<LastInject>,
    capture_active: &AtomicBool,
    poll: TypedLearningPoll,
) -> TypedLearningDecision {
    match poll {
        TypedLearningPoll::Wait => TypedLearningDecision::Wait,
        TypedLearningPoll::Close => {
            *slot = None;
            capture_active.store(false, Ordering::Release);
            TypedLearningDecision::Stop
        }
        TypedLearningPoll::Learn(pairs) => {
            *slot = None;
            capture_active.store(false, Ordering::Release);
            TypedLearningDecision::Persist(pairs)
        }
    }
}

fn maybe_learn_typed_correction(ctx: &EngineCtx, current: &crate::app_context::AppContext) {
    if !ctx.correction_capture_active.load(Ordering::Acquire) {
        return;
    }
    let now = Instant::now();
    let decision = {
        let mut slot = ctx.last_inject.lock();
        let poll = match slot.as_mut() {
            Some(last) => typed_learning_poll(last, current, now),
            None => TypedLearningPoll::Close,
        };
        consume_typed_learning_poll_locked(&mut slot, ctx.correction_capture_active.as_ref(), poll)
    };
    let pairs = match decision {
        TypedLearningDecision::Wait | TypedLearningDecision::Stop => return,
        TypedLearningDecision::Persist(pairs) => pairs,
    };
    if pairs.is_empty() {
        return;
    }

    let conn = ctx.db.lock();
    let mut learned = 0u32;
    for (wrong, right) in pairs {
        if db::add_correction(&conn, &wrong, &right).is_ok() {
            learned += 1;
        }
    }
    if learned > 0 {
        dbg_log(&format!("выучено исправлений: {learned}"));
        let _ = ctx
            .app
            .emit("learned", serde_json::json!({ "count": learned }));
    }
}

fn typed_learning_poll(
    last: &mut LastInject,
    current: &crate::app_context::AppContext,
    now: Instant,
) -> TypedLearningPoll {
    let age = now.saturating_duration_since(last.at);
    if age > TYPED_CORRECTION_WINDOW || !last.target_fp.matches(current) {
        return TypedLearningPoll::Close;
    }
    let Some(manual_edit_at) = last.manual_edit_at else {
        return TypedLearningPoll::Wait;
    };
    if now.saturating_duration_since(manual_edit_at) < TYPED_CORRECTION_DEBOUNCE {
        return TypedLearningPoll::Wait;
    }
    if !field_metadata_allows_learning(&last.field_role, &last.field_subrole)
        || !field_metadata_allows_learning(&current.field_role, &current.field_subrole)
    {
        return TypedLearningPoll::Close;
    }

    // An explicit field id is strongest. When a provider exposes no id, a
    // non-empty pre-insert tail becomes the stable anchor; an initially empty,
    // anonymous field is intentionally not learned from.
    let same_known_field = !last.field_id.is_empty()
        && !current.field_id.is_empty()
        && last.field_id == current.field_id;
    let anchored_anonymous_field = !last.field_before.is_empty();
    if !same_known_field && !anchored_anonymous_field {
        return TypedLearningPoll::Close;
    }

    let Some(observed) = current.focused_text_tail(TYPED_CORRECTION_FIELD_LIMIT) else {
        return TypedLearningPoll::Close;
    };
    match last.pending_observation.as_mut() {
        Some(pending) if pending.edit_epoch == manual_edit_at && pending.field_text == observed => {
            if now.saturating_duration_since(pending.observed_at) < TYPED_CORRECTION_CONFIRM {
                return TypedLearningPoll::Wait;
            }
        }
        _ => {
            last.pending_observation = Some(TypedCorrectionObservation {
                field_text: observed,
                edit_epoch: manual_edit_at,
                observed_at: now,
            });
            return TypedLearningPoll::Wait;
        }
    }
    let observed = last
        .pending_observation
        .take()
        .map(|pending| pending.field_text)
        .unwrap_or_default();
    TypedLearningPoll::Learn(typed_correction_pairs_from_field(
        &last.text,
        &last.field_before,
        &observed,
        true,
        age,
    ))
}

fn field_metadata_allows_learning(role: &str, subrole: &str) -> bool {
    let metadata = format!("{role} {subrole}").to_lowercase();
    if ["password", "secure", "credential", "парол", "защищ"]
        .iter()
        .any(|marker| metadata.contains(marker))
    {
        return false;
    }
    ["text", "edit", "document"]
        .iter()
        .any(|marker| metadata.contains(marker))
}

fn compact_learning_text(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn typed_correction_pairs_from_field(
    injected: &str,
    field_before: &str,
    observed_field: &str,
    same_target: bool,
    age: Duration,
) -> Vec<(String, String)> {
    if !same_target || age > TYPED_CORRECTION_WINDOW {
        return Vec::new();
    }
    let Some(edited) = edited_inserted_segment(field_before, observed_field) else {
        return Vec::new();
    };
    if contains_likely_secret(injected) || contains_likely_secret(&edited) {
        return Vec::new();
    }
    learned_correction_pairs(injected, &edited, age)
}

fn edited_inserted_segment(field_before: &str, observed_field: &str) -> Option<String> {
    let before = compact_learning_text(field_before);
    let observed = compact_learning_text(observed_field);
    if observed.is_empty() {
        return None;
    }
    if before.is_empty() {
        return Some(observed);
    }
    if let Some(rest) = observed.strip_prefix(&before) {
        let rest = rest.trim();
        return (!rest.is_empty()).then(|| rest.to_string());
    }

    // AX providers commonly expose the whole field, so dictation inserted at
    // a caret in the middle appears between an unchanged prefix and suffix.
    // Extract only a strongly two-sided region; weak/ambiguous anchors fail
    // closed and cannot create a global correction rule.
    if let Some(middle) = middle_inserted_segment(&before, &observed) {
        return Some(middle);
    }

    // Both detector implementations return a bounded tail. A long pre-existing
    // document may therefore lose its beginning after insertion; match only a
    // sufficiently long suffix anchor and reject the field if it disappeared.
    let anchor: String = before
        .chars()
        .rev()
        .take(96)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    if anchor.chars().count() < 12 {
        return None;
    }
    let index = observed.rfind(&anchor)?;
    let rest = observed[index + anchor.len()..].trim();
    (!rest.is_empty()).then(|| rest.to_string())
}

fn middle_inserted_segment(before: &str, observed: &str) -> Option<String> {
    let before_chars = before.chars().collect::<Vec<_>>();
    let observed_chars = observed.chars().collect::<Vec<_>>();
    let mut prefix = 0usize;
    while prefix < before_chars.len()
        && prefix < observed_chars.len()
        && before_chars[prefix] == observed_chars[prefix]
    {
        prefix += 1;
    }

    let mut suffix = 0usize;
    while suffix < before_chars.len().saturating_sub(prefix)
        && suffix < observed_chars.len().saturating_sub(prefix)
        && before_chars[before_chars.len() - 1 - suffix]
            == observed_chars[observed_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    if prefix == 0 || suffix == 0 {
        return None;
    }

    let left_strength = before_chars[..prefix]
        .iter()
        .filter(|ch| ch.is_alphanumeric())
        .count();
    let right_strength = before_chars[before_chars.len() - suffix..]
        .iter()
        .filter(|ch| ch.is_alphanumeric())
        .count();
    if left_strength < 3 || right_strength < 3 || left_strength + right_strength < 8 {
        return None;
    }

    let middle = observed_chars[prefix..observed_chars.len() - suffix]
        .iter()
        .collect::<String>();
    let middle = middle.trim();
    (!middle.is_empty()).then(|| middle.to_string())
}

fn contains_likely_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    if ["-----begin ", "ghp_", "github_pat_", "sk-", "eyj", "aiza"]
        .iter()
        .any(|prefix| lower.contains(prefix))
    {
        return true;
    }
    value.split_whitespace().any(|token| {
        let compact: String = token
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        let has_letter = compact.chars().any(|c| c.is_ascii_alphabetic());
        let has_digit = compact.chars().any(|c| c.is_ascii_digit());
        let digit_count = compact.chars().filter(|c| c.is_ascii_digit()).count();
        (compact.len() >= 20 && has_letter && has_digit) || (13..=19).contains(&digit_count)
    })
}

#[derive(Clone, Debug)]
struct LearnToken {
    raw: String,
    norm: String,
}

fn learned_correction_pairs(injected: &str, edited: &str, age: Duration) -> Vec<(String, String)> {
    if age > TYPED_CORRECTION_WINDOW {
        return Vec::new();
    }
    let wrong = learning_tokens(injected);
    let right = learning_tokens(edited);
    if wrong.is_empty() || right.is_empty() {
        return Vec::new();
    }
    if token_norms_equal(&wrong, &right) {
        let pairs = wrong
            .iter()
            .zip(&right)
            .filter(|(before, after)| before.raw != after.raw)
            .map(|(before, after)| (before.raw.clone(), after.raw.clone()))
            .collect();
        return dedup_learned_pairs(pairs);
    }
    if wrong.len() > 80 || right.len() > 80 {
        return Vec::new();
    }

    let anchors = lcs_token_anchors(&wrong, &right);
    if !correction_edit_is_related(&wrong, &right, &anchors) {
        return Vec::new();
    }
    let mut out = Vec::new();
    if anchors.is_empty() {
        push_learned_span(&mut out, &wrong, &right, SpanBounds::None, age);
        return dedup_learned_pairs(out);
    }

    let mut prev_w = 0usize;
    let mut prev_r = 0usize;
    let mut has_left_anchor = false;
    for (wi, ri) in anchors
        .into_iter()
        .chain(std::iter::once((wrong.len(), right.len())))
    {
        let has_right_anchor = wi < wrong.len() && ri < right.len();
        let bounds = match (has_left_anchor, has_right_anchor) {
            (true, true) => SpanBounds::Both,
            (true, false) | (false, true) => SpanBounds::OneSided,
            (false, false) => SpanBounds::None,
        };
        push_learned_span(
            &mut out,
            &wrong[prev_w..wi],
            &right[prev_r..ri],
            bounds,
            age,
        );
        prev_w = wi.saturating_add(1);
        prev_r = ri.saturating_add(1);
        has_left_anchor |= has_right_anchor;
    }
    dedup_learned_pairs(out)
}

fn correction_edit_is_related(
    wrong: &[LearnToken],
    right: &[LearnToken],
    anchors: &[(usize, usize)],
) -> bool {
    let min_tokens = wrong.len().min(right.len());
    let max_tokens = wrong.len().max(right.len());
    if min_tokens == 0
        || max_tokens.saturating_sub(min_tokens) > 2
        || max_tokens > min_tokens.saturating_mul(2).saturating_add(1)
    {
        return false;
    }
    let similarity = correction_similarity(&join_learn_tokens(wrong), &join_learn_tokens(right));
    if anchors.is_empty() {
        return max_tokens <= 6 && similarity >= 0.30;
    }
    let anchor_ratio = anchors.len() as f32 / min_tokens as f32;
    anchor_ratio >= 0.40 || similarity >= 0.35
}

fn learning_tokens(text: &str) -> Vec<LearnToken> {
    text.split_whitespace()
        .filter_map(|raw| {
            let clean = raw
                .trim_matches(|c: char| !c.is_alphanumeric())
                .trim_matches(|c: char| !c.is_alphanumeric());
            if clean.is_empty() {
                return None;
            }
            Some(LearnToken {
                raw: clean.to_string(),
                norm: clean.to_lowercase(),
            })
        })
        .collect()
}

fn token_norms_equal(a: &[LearnToken], b: &[LearnToken]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.norm == y.norm)
}

fn lcs_token_anchors(a: &[LearnToken], b: &[LearnToken]) -> Vec<(usize, usize)> {
    let m = a.len();
    let n = b.len();
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i][j] = if a[i].norm == b[j].norm {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut anchors = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < m && j < n {
        if a[i].norm == b[j].norm {
            anchors.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    anchors
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SpanBounds {
    None,
    OneSided,
    Both,
}

fn push_learned_span(
    out: &mut Vec<(String, String)>,
    wrong: &[LearnToken],
    right: &[LearnToken],
    bounds: SpanBounds,
    age: Duration,
) {
    // Accessibility APIs can return only the text before the caret.  In that
    // case a corrected final token looks like a many-to-one replacement that
    // also swallowed the (unobserved) suffix.  Learn only replacement spans
    // with the same token count: this deliberately fails closed instead of
    // teaching a destructive rule from a truncated field snapshot.
    if wrong.is_empty() || right.is_empty() || wrong.len() != right.len() {
        return;
    }
    // A multi-token edit with an unchanged anchor on only one side is
    // indistinguishable from a before-caret snapshot that silently omitted
    // the rest of the sentence. Whole-utterance corrections (no anchors),
    // single-word edits, and phrases bounded on both sides remain learnable.
    if bounds == SpanBounds::OneSided && wrong.len() > 1 {
        return;
    }
    let anchored = bounds != SpanBounds::None;
    if span_pair_valid(wrong, right, anchored, age) {
        out.push((join_learn_tokens(wrong), join_learn_tokens(right)));
    }
    for (w, r) in wrong.iter().zip(right) {
        if span_pair_valid(
            std::slice::from_ref(w),
            std::slice::from_ref(r),
            anchored,
            age,
        ) {
            out.push((w.raw.clone(), r.raw.clone()));
        }
    }
}

fn span_pair_valid(
    wrong: &[LearnToken],
    right: &[LearnToken],
    anchored: bool,
    age: Duration,
) -> bool {
    if wrong.len() > 6 || right.len() > 6 {
        return false;
    }
    let w = join_learn_tokens(wrong);
    let r = join_learn_tokens(right);
    let wc = w.chars().count();
    let rc = r.chars().count();
    if wc < 2 || rc < 2 || wc > 80 || rc > 80 || w.eq_ignore_ascii_case(&r) {
        return false;
    }
    if anchored {
        // Stable surrounding words localize the edit, but a completely
        // unrelated replacement is usually a content rewrite, not an ASR
        // correction. Some Windows TextPattern providers expose only the text
        // before the caret: after a mid-sentence edit, the unseen original
        // suffix must never be absorbed into a learned phrase. Requiring
        // one-to-one, individually related tokens keeps the real correction
        // while dropping that truncated suffix.
        return wrong.len() == right.len()
            && wrong
                .iter()
                .zip(right)
                .all(|(before, after)| correction_similarity(&before.raw, &after.raw) >= 0.20);
    }
    if age > Duration::from_secs(3 * 60) {
        return false;
    }
    correction_similarity(&w, &r) >= 0.30
}

fn correction_similarity(wrong: &str, right: &str) -> f32 {
    let a = rough_latin_key(wrong);
    let b = rough_latin_key(right);
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let dist = levenshtein_chars(&a, &b);
    1.0 - (dist as f32 / a.chars().count().max(b.chars().count()) as f32)
}

fn rough_latin_key(value: &str) -> String {
    let mut out = String::new();
    for ch in value.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            continue;
        }
        let mapped = match ch {
            'а' => "a",
            'б' => "b",
            'в' => "v",
            'г' => "g",
            'д' => "d",
            'е' | 'э' => "e",
            'ё' => "yo",
            'ж' => "zh",
            'з' => "z",
            'и' => "i",
            'й' => "y",
            'к' => "k",
            'л' => "l",
            'м' => "m",
            'н' => "n",
            'о' => "o",
            'п' => "p",
            'р' => "r",
            'с' => "s",
            'т' => "t",
            'у' => "u",
            'ф' => "f",
            'х' => "h",
            'ц' => "ts",
            'ч' => "ch",
            'ш' => "sh",
            'щ' => "shch",
            'ы' => "y",
            'ю' => "yu",
            'я' => "ya",
            'ъ' | 'ь' => "",
            _ => "",
        };
        out.push_str(mapped);
    }
    out
}

fn levenshtein_chars(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut cur = vec![0usize; b_chars.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b_chars.len()]
}

fn join_learn_tokens(tokens: &[LearnToken]) -> String {
    tokens
        .iter()
        .map(|t| t.raw.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

fn dedup_learned_pairs(pairs: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (wrong, right) in pairs {
        let key = (wrong.to_lowercase(), right.to_lowercase());
        if seen.insert(key) {
            out.push((wrong, right));
        }
    }
    out
}

#[cfg(test)]
mod correction_learning_tests {
    use super::*;

    fn has_pair(pairs: &[(String, String)], wrong: &str, right: &str) -> bool {
        pairs.iter().any(|(w, r)| w == wrong && r == right)
    }

    fn field_context(
        exe: &str,
        window_id: &str,
        field_id: &str,
        text: &str,
    ) -> crate::app_context::AppContext {
        crate::app_context::AppContext {
            exe: exe.into(),
            title: "Editor".into(),
            window_id: window_id.into(),
            category: "work".into(),
            field_role: "UIAEdit".into(),
            field_subrole: "textpattern".into(),
            field_id: field_id.into(),
            field_text: text.into(),
            selected_text: String::new(),
        }
    }

    fn last_inject(
        context: &crate::app_context::AppContext,
        text: &str,
        before: &str,
        at: Instant,
    ) -> LastInject {
        LastInject {
            text: text.into(),
            at,
            target_fp: context.target_fingerprint(),
            field_id: context.field_id.clone(),
            field_role: context.field_role.clone(),
            field_subrole: context.field_subrole.clone(),
            field_before: before.into(),
            manual_edit_at: Some(at + Duration::from_millis(100)),
            pending_observation: None,
        }
    }

    #[test]
    fn learns_whole_brand_phrase_without_shared_tokens() {
        let pairs = learned_correction_pairs("Виспа Фолл", "Wispr Flow", Duration::from_secs(30));

        assert!(has_pair(&pairs, "Виспа Фолл", "Wispr Flow"));
    }

    #[test]
    fn learns_changed_phrase_between_stable_anchors() {
        let pairs = learned_correction_pairs(
            "открой Виспа Фолл пожалуйста",
            "открой Wispr Flow пожалуйста",
            Duration::from_secs(90),
        );

        assert!(has_pair(&pairs, "Виспа Фолл", "Wispr Flow"));
    }

    #[test]
    fn learns_short_phonetic_single_word_correction() {
        let pairs = learned_correction_pairs("фу", "foo", Duration::from_secs(15));

        assert!(has_pair(&pairs, "фу", "foo"));
    }

    #[test]
    fn ordinary_backspace_and_typed_replacement_learns_without_clipboard() {
        let at = Instant::now();
        let start = field_context("editor.exe", "window-1", "field-1", "");
        let mut last = last_inject(&start, "виспа", "", at);
        // This is the Accessibility value after ordinary Backspace + typing;
        // no clipboard value participates in the decision.
        let current = field_context("editor.exe", "window-1", "field-1", "Wispr");

        assert!(matches!(
            typed_learning_poll(&mut last, &current, at + Duration::from_secs(1)),
            TypedLearningPoll::Wait
        ));
        let TypedLearningPoll::Learn(pairs) =
            typed_learning_poll(&mut last, &current, at + Duration::from_millis(1_400))
        else {
            panic!("stable typed edit must be evaluated");
        };
        assert!(has_pair(&pairs, "виспа", "Wispr"));
    }

    #[test]
    fn partial_word_snapshot_never_learns_before_the_final_edit_epoch() {
        let at = Instant::now();
        let start = field_context("editor.exe", "window-1", "field-1", "");
        let mut last = last_inject(&start, "cat", "", at);
        let partial = field_context("editor.exe", "window-1", "field-1", "ca");

        // The first post-debounce snapshot is only a candidate; it cannot
        // persist cat -> ca on its own.
        assert!(matches!(
            typed_learning_poll(&mut last, &partial, at + Duration::from_secs(1)),
            TypedLearningPoll::Wait
        ));

        // The final `r` is a new typing epoch and invalidates the partial
        // candidate before the next Accessibility poll.
        last.mark_manual_edit(at + Duration::from_millis(1_100));
        let final_text = field_context("editor.exe", "window-1", "field-1", "car");
        assert!(matches!(
            typed_learning_poll(&mut last, &final_text, at + Duration::from_millis(1_900)),
            TypedLearningPoll::Wait
        ));
        let TypedLearningPoll::Learn(pairs) =
            typed_learning_poll(&mut last, &final_text, at + Duration::from_millis(2_300))
        else {
            panic!("two stable final snapshots must be learned");
        };
        assert!(has_pair(&pairs, "cat", "car"), "{pairs:?}");
        assert!(!has_pair(&pairs, "cat", "ca"), "{pairs:?}");
    }

    #[test]
    fn terminal_learning_poll_consumes_capture_in_one_locked_step() {
        let at = Instant::now();
        let start = field_context("editor.exe", "window-1", "field-1", "");
        let active = AtomicBool::new(true);
        let mut slot = Some(last_inject(&start, "cat", "", at));

        assert!(matches!(
            consume_typed_learning_poll_locked(&mut slot, &active, TypedLearningPoll::Wait),
            TypedLearningDecision::Wait
        ));
        assert!(slot.is_some());
        assert!(active.load(Ordering::Acquire));

        let decision = consume_typed_learning_poll_locked(
            &mut slot,
            &active,
            TypedLearningPoll::Learn(vec![("cat".into(), "car".into())]),
        );
        let TypedLearningDecision::Persist(pairs) = decision else {
            panic!("learn must return the pairs for persistence");
        };
        assert!(has_pair(&pairs, "cat", "car"));
        assert!(slot.is_none());
        assert!(!active.load(Ordering::Acquire));

        active.store(true, Ordering::Release);
        slot = Some(last_inject(&start, "cat", "", at));
        assert!(matches!(
            consume_typed_learning_poll_locked(&mut slot, &active, TypedLearningPoll::Close),
            TypedLearningDecision::Stop
        ));
        assert!(slot.is_none());
        assert!(!active.load(Ordering::Acquire));
    }

    #[test]
    fn live_reconciled_final_uses_pre_dictation_prefix_for_phrase_edit() {
        let pairs = typed_correction_pairs_from_field(
            "открой Виспа Фолл пожалуйста",
            "Черновик: ",
            "Черновик: открой Wispr Flow пожалуйста",
            true,
            Duration::from_secs(20),
        );
        assert!(has_pair(&pairs, "Виспа Фолл", "Wispr Flow"));
    }

    #[test]
    fn correction_inserted_at_middle_caret_uses_unchanged_prefix_and_suffix() {
        let pairs = typed_correction_pairs_from_field(
            "cat",
            "hello  tail",
            "hello car tail",
            true,
            Duration::from_secs(10),
        );
        assert!(has_pair(&pairs, "cat", "car"), "{pairs:?}");
    }

    #[test]
    fn case_only_brand_edit_is_learned() {
        let pairs = typed_correction_pairs_from_field(
            "wispr flow",
            "",
            "Wispr Flow",
            true,
            Duration::from_secs(10),
        );
        assert!(has_pair(&pairs, "wispr", "Wispr"));
        assert!(has_pair(&pairs, "flow", "Flow"));
    }

    #[test]
    fn changed_window_or_field_closes_learning_window() {
        let at = Instant::now();
        let start = field_context("editor.exe", "window-1", "field-1", "");
        let mut last = last_inject(&start, "виспа", "", at);
        let other_window = field_context("browser.exe", "window-2", "field-1", "Wispr");
        let other_field = field_context("editor.exe", "window-1", "field-2", "Wispr");

        assert!(matches!(
            typed_learning_poll(&mut last, &other_window, at + Duration::from_secs(1)),
            TypedLearningPoll::Close
        ));
        assert!(matches!(
            typed_learning_poll(&mut last, &other_field, at + Duration::from_secs(1)),
            TypedLearningPoll::Close
        ));
    }

    #[test]
    fn secure_field_closes_without_reading_or_learning() {
        let at = Instant::now();
        let start = field_context("browser.exe", "window-1", "field-1", "");
        let mut last = last_inject(&start, "секрет", "", at);
        let mut secure = field_context("browser.exe", "window-1", "field-1", "do-not-store");
        secure.field_subrole = "password secure credential".into();

        assert!(matches!(
            typed_learning_poll(&mut last, &secure, at + Duration::from_secs(1)),
            TypedLearningPoll::Close
        ));
    }

    #[test]
    fn rejects_unrelated_new_text() {
        let pairs = learned_correction_pairs("привет", "password", Duration::from_secs(15));

        assert!(pairs.is_empty(), "{pairs:?}");
    }

    #[test]
    fn rejects_stale_typed_edit() {
        let pairs = learned_correction_pairs("Виспа Фолл", "Wispr Flow", Duration::from_secs(121));

        assert!(pairs.is_empty());
    }

    #[test]
    fn plain_append_is_not_mistaken_for_phrase_correction() {
        let pairs = typed_correction_pairs_from_field(
            "Готово",
            "",
            "Готово, отправь дальше",
            true,
            Duration::from_secs(10),
        );
        assert!(pairs.is_empty());
    }

    #[test]
    fn truncated_before_caret_snapshot_is_not_learned_as_a_phrase_replacement() {
        let pairs = typed_correction_pairs_from_field(
            "открой виспа фолл пожалуйста сейчас",
            "",
            "открой Wispr",
            true,
            Duration::from_secs(10),
        );
        assert!(pairs.is_empty());
    }

    #[test]
    fn one_token_truncated_before_caret_snapshot_is_not_overlearned() {
        let pairs = typed_correction_pairs_from_field(
            "пожалуйста открой виспа фолл сейчас",
            "",
            "пожалуйста открой Wispr Flow",
            true,
            Duration::from_secs(10),
        );
        assert!(pairs.is_empty(), "{pairs:?}");
    }

    #[test]
    fn balanced_before_caret_snapshot_is_not_overlearned() {
        let pairs = typed_correction_pairs_from_field(
            "пожалуйста открой виспа сейчас",
            "",
            "пожалуйста открой Wispr Flow",
            true,
            Duration::from_secs(10),
        );
        assert!(pairs.is_empty(), "{pairs:?}");
    }

    #[test]
    fn one_sided_single_word_correction_remains_learnable() {
        let pairs = typed_correction_pairs_from_field(
            "открой виспа",
            "",
            "открой Wispr",
            true,
            Duration::from_secs(10),
        );
        assert!(has_pair(&pairs, "виспа", "Wispr"), "{pairs:?}");
    }

    #[test]
    fn corrected_middle_word_with_hidden_suffix_is_not_overlearned() {
        let pairs = typed_correction_pairs_from_field(
            "hello cat tail",
            "",
            "hello car",
            true,
            Duration::from_secs(10),
        );
        assert!(pairs.is_empty(), "{pairs:?}");
    }

    #[test]
    fn unchanged_field_is_a_noop() {
        let pairs = typed_correction_pairs_from_field(
            "Wispr Flow",
            "Черновик: ",
            "Черновик: Wispr Flow",
            true,
            Duration::from_secs(10),
        );
        assert!(pairs.is_empty());
    }

    #[test]
    fn likely_secret_tokens_are_never_learned() {
        let pairs = typed_correction_pairs_from_field(
            "токен",
            "",
            "ghp_1234567890abcdefghijklmnop",
            true,
            Duration::from_secs(10),
        );
        assert!(pairs.is_empty());
    }
}
