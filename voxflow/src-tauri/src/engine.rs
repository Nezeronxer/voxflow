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
    Stop,
    Toggle,
    Cancel,
    ImproveSelection,
    HotkeyLatch,
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
}

impl EngineHandle {
    pub fn restore_auto_mute(&self) {
        restore_auto_mute_arc(&self.auto_mute);
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
    /// Хэндл потока петли — чтобы дождаться его в stop (ни один тик не идёт во время финала).
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
}

#[derive(Default)]
struct DictationMemory {
    target_fp: Option<(String, String)>,
    summary: String,
    recent: VecDeque<String>,
}

#[derive(Clone)]
struct LastInject {
    text: String,
    at: Instant,
}

#[derive(Clone)]
struct EngineCtx {
    app: AppHandle,
    db: Arc<Mutex<Connection>>,
    settings: Arc<Mutex<Settings>>,
    recording: Arc<AtomicBool>,
    /// Постоянный whisper-server (если используется движок whisper_server).
    server: Arc<Mutex<Option<asr::Server>>>,
    /// Последний вставленный текст — для авто-захвата исправлений из буфера обмена.
    last_inject: Arc<Mutex<Option<LastInject>>>,
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
    let ctx = EngineCtx {
        app,
        db,
        settings,
        recording,
        server: Arc::new(Mutex::new(None)),
        last_inject: Arc::new(Mutex::new(None)),
        asr_lock: Arc::new(Mutex::new(())),
        inject_lock: Arc::new(Mutex::new(())),
        partial: Arc::new(Mutex::new(None)),
        gen: Arc::new(AtomicU64::new(0)),
        last_injected_gen: Arc::new(AtomicU64::new(0)),
        gigaam: Arc::new(Mutex::new(None)),
        parakeet: Arc::new(Mutex::new(None)),
        vad: Arc::new(Mutex::new(None)),
        vad_final: Arc::new(Mutex::new(None)),
        improve_busy: Arc::new(AtomicBool::new(false)),
        dictation_memory: Arc::new(Mutex::new(DictationMemory::default())),
        auto_mute: auto_mute.clone(),
    };
    // Прогрев whisper-server в фоне (CUDA JIT один раз → первая диктовка тоже быстрая).
    let warm = ctx.clone();
    std::thread::spawn(move || warmup(warm));
    // Монитор буфера обмена — авто-захват исправлений пользователя.
    let mon = ctx.clone();
    std::thread::spawn(move || clipboard_monitor(mon));
    std::thread::Builder::new()
        .name("voxflow-engine".into())
        .spawn(move || engine_loop(rx, ctx))
        .expect("spawn engine thread");
    EngineHandle { auto_mute }
}

/// Простой файловый лог для диагностики (data_dir/debug.log).
pub fn dbg_log(msg: &str) {
    use std::io::Write;
    let p = paths::data_dir().join("debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
    {
        let now = chrono::Local::now().format("%H:%M:%S%.3f");
        let _ = writeln!(f, "[{now}] {msg}");
    }
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

/// Маршрут локального распознавания по настройкам (роутер языков, PLAN §2).
/// Считается заново на каждый старт/финал — установка модели Parakeet
/// подхватывается без перезапуска.
#[derive(Clone, Copy, PartialEq, Debug)]
enum LocalRoute {
    /// ru + движок gigaam — как раньше.
    GigaAm,
    /// en/auto при установленном Parakeet.
    Parakeet,
    /// Всё остальное (auto/прочие языки/whisper-движки).
    Whisper,
}

fn local_route(s: &Settings) -> LocalRoute {
    let parakeet_ready = crate::parakeet::dir_ready(&paths::parakeet_dir());
    local_route_with_parakeet(s, parakeet_ready)
}

fn local_route_with_parakeet(s: &Settings, parakeet_ready: bool) -> LocalRoute {
    match s.language.as_str() {
        "ru" if s.engine == "gigaam" => LocalRoute::GigaAm,
        "en" if s.engine == "gigaam" && parakeet_ready => LocalRoute::Parakeet,
        "auto" if s.engine == "gigaam" && parakeet_ready => LocalRoute::Parakeet,
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
        return gw + 2 >= ww;
    }
    // Типичный сбой whisper auto на русской речи: короткая латинская фраза
    // вроде "After" / "Państwo, unze" вместо полноценной русской диктовки.
    !has_cyrillic(w) && gw >= 3 && (ww <= 2 || gw >= ww.saturating_mul(2))
}

/// Заранее поднять и прогреть резидентные модели (GigaAM/Parakeet/VAD или
/// whisper-server), чтобы первая диктовка не ждала загрузку/JIT.
fn warmup(ctx: EngineCtx) {
    std::thread::sleep(Duration::from_millis(1200));
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
    let whisper_dir = paths::whisper_dir(&ctx.app);
    dbg_log(&format!("warmup: whisper_dir={whisper_dir:?}"));
    dbg_log(&format!("warmup: model={model:?}"));
    let wav = paths::tmp_dir().join("warmup.wav");
    if let Err(e) = audio::write_wav_16k_mono(&wav, &vec![0.0f32; 8000]) {
        dbg_log(&format!("warmup: wav ОШИБКА: {e}"));
        return;
    }
    match ensure_server(&ctx, &whisper_dir, &model, s.effective_threads()) {
        Ok(port) => {
            dbg_log(&format!("warmup: сервер на {port}, прогрев..."));
            let r = asr::transcribe_server(port, &wav, &s.language, None);
            dbg_log(&format!("warmup: прогрет ok={}", r.is_ok()));
        }
        Err(e) => dbg_log(&format!("warmup: ensure_server ОШИБКА: {e:#}")),
    }
}

fn engine_loop(rx: Receiver<EngineCmd>, ctx: EngineCtx) {
    // Capture (cpal Stream) создаётся и уничтожается только здесь — он !Send.
    let mut capture: Option<Capture> = None;
    while let Ok(cmd) = rx.recv() {
        match cmd {
            EngineCmd::Start => start_capture_into(&mut capture, &ctx),
            EngineCmd::Stop => stop_and_process(&mut capture, &ctx),
            EngineCmd::Toggle => {
                if capture.is_some() {
                    stop_and_process(&mut capture, &ctx);
                } else {
                    start_capture_into(&mut capture, &ctx);
                }
            }
            EngineCmd::Cancel => cancel_current(&mut capture, &ctx),
            EngineCmd::ImproveSelection => improve_selection(&ctx),
            EngineCmd::HotkeyLatch => notify_hotkey_latch(&ctx, capture.is_some()),
            EngineCmd::Warmup => {
                // В отдельном потоке: warmup сам спит и грузит модели —
                // канал команд блокировать нельзя (Start/Stop должны жить).
                let wctx = ctx.clone();
                std::thread::spawn(move || warmup(wctx));
            }
            EngineCmd::Shutdown => {
                restore_auto_mute(&ctx);
                if let Some(mut srv) = ctx.server.lock().take() {
                    let _ = srv.child.kill();
                }
                break;
            }
        }
    }
}

fn notify_hotkey_latch(ctx: &EngineCtx, active: bool) {
    if !active {
        dbg_log("hotkey: double-press latch ignored because capture is not active");
        return;
    }
    if ctx.settings.lock().play_sounds {
        sound::latch();
    }
    let _ = ctx.app.emit(
        "hotkey_latch",
        serde_json::json!({
            "message": "Режим без удержания",
            "detail": "Двойное нажатие"
        }),
    );
    dbg_log("hotkey: double-press latch enabled");
}

fn start_capture_into(capture: &mut Option<Capture>, ctx: &EngineCtx) {
    if capture.is_some() {
        return;
    }
    let (device, play, auto_mute) = {
        let s = ctx.settings.lock();
        (s.input_device.clone(), s.play_sounds, s.auto_mute)
    };
    // B3: для локального распознавания модель обязательна — без неё НЕ начинаем «запись в
    // никуда», а сразу показываем предупреждение «Выберите модель». Облачный ASR
    // (Gemini-транскрипция ИЛИ облачный STT-провайдер) модель не требует, поэтому
    // проверяем только для чисто локального пути.
    {
        let s = ctx.settings.lock();
        // Облако «активно» только при наличии ключа — иначе провайдер openai_compat/deepgram
        // de-facto уходит в локальное распознавание (умный фолбэк). Та же проверка, что и в
        // process_utterance (общий хелпер cloud_stt_active): без ключа провайдер облачный,
        // но модель нам ВСЁ РАВНО нужна, иначе гард «выберите модель» пропустили бы и юзер
        // записал бы «в никуда» (баг старта).
        let use_cloud_stt = s.cloud_stt_active();
        let use_cloud_gemini =
            s.cloud_asr && s.ai_backend == "gemini" && crate::gemini::available(&s.ai_api_key);
        let use_cloud = use_cloud_stt || use_cloud_gemini;
        if !use_cloud && no_model_installed(&s) {
            drop(s);
            dbg_log("start: модель не установлена — запись не начинаем, предупреждаем");
            emit_no_model(&ctx.app);
            set_status(&ctx.app, "idle");
            return;
        }
    }
    match audio::start_capture(&device) {
        Ok(c) => {
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
            // Новая диктовка → новое поколение (C4): осиротевшие финал-потоки прошлых
            // диктовок увидят расхождение gen и не станут вставлять повторно.
            ctx.gen.fetch_add(1, Ordering::SeqCst);
            set_status(&ctx.app, "recording");
            // Запускаем петлю живого стриминга, если GPU whisper-server доступен —
            // пилюля показывает живой текст и при whisper_cli (живой инжект при этом
            // выключен, см. maybe_start_partial_loop). Без GPU/модели — статичное «Слушаю…».
            maybe_start_partial_loop(&c, ctx);
            // Петля уровня громкости для orb-визуализатора (событие "level").
            spawn_level_loop(&c, ctx);
            *capture = Some(c);
        }
        Err(err) => {
            log::error!("start_capture: {err:#}");
            emit_error(&ctx.app, &format!("Не удалось открыть микрофон: {err}"));
            set_status(&ctx.app, "idle");
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
fn maybe_start_partial_loop(capture: &Capture, ctx: &EngineCtx) {
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
            start_cloud_partial_loop(capture, ctx, &s);
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
        if ensure_gigaam(ctx, &s).is_ok() {
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
                Arc::clone(&ctx.gigaam),
                LocalLoopTuning {
                    tick_ms: 220,
                    max_seg_samples: 25 * 16000,
                    fixed_lang: Some("ru"),
                },
            );
            return;
        }
        dbg_log("partial: auto+gigaam preview недоступен — пробуем whisper-стрим");
    }
    match local_route(&s) {
        LocalRoute::GigaAm => {
            if ensure_gigaam(ctx, &s).is_err() {
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
                Arc::clone(&ctx.gigaam),
                LocalLoopTuning {
                    tick_ms: 350,
                    max_seg_samples: 25 * 16000,
                    fixed_lang: Some("ru"),
                },
            );
            return;
        }
        LocalRoute::Parakeet => {
            if ensure_parakeet(ctx, &s).is_ok() {
                // en/auto: партиалы гонит Parakeet БЕЗ двойного прогона (RU-перегон
                // кириллических сегментов — только в финале); язык бейджа
                // определяется по скрипту текущего текста.
                start_local_partial_loop(
                    capture,
                    ctx,
                    &s,
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
    // Живой стрим требует GPU whisper-server (CPU-сервер слишком медленный для тиков).
    if !paths::has_nvidia() {
        dbg_log("partial: нет NVIDIA GPU — без живого стрима (пилюля статична)");
        return;
    }

    // Прогреваем сервер и получаем порт ДО спавна, чтобы первый тик не ждал JIT.
    let whisper_dir = paths::whisper_dir(&ctx.app);
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
    let port = match ensure_server(ctx, &whisper_dir, &model, s.effective_threads()) {
        Ok(p) => p,
        Err(e) => {
            dbg_log(&format!(
                "partial: ensure_server ошибка: {e:#} — без стриминга"
            ));
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
    let actx = crate::app_context::detect();
    let start_fp = actx.target_fingerprint();
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
    let lang = s.language.clone();
    let mode = effective_mode;
    let settings = s.clone();
    let (dict, snippets, corrections) = load_live_postprocess_data(ctx);

    // Клоны Arc для потока (originals остаются в PartialState).
    let t_stop = Arc::clone(&stop);
    let t_abort = Arc::clone(&abort);
    let t_injected = Arc::clone(&injected);
    let t_committed = Arc::clone(&committed);
    let t_fp = start_fp.clone();
    let t_mode = mode.clone();

    let join = std::thread::Builder::new()
        .name("voxflow-partial".into())
        .spawn(move || {
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
fn start_cloud_partial_loop(capture: &Capture, ctx: &EngineCtx, s: &Settings) {
    let actx = crate::app_context::detect();
    let start_fp = actx.target_fingerprint();
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
        let text = match crate::cloud_stt::transcribe(&s, &wav) {
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

/// Каденс/лимиты петли локальных партиалов: GigaAM — тик 350 мс и кап сегмента
/// 25 c (лимит pos_emb модели), Parakeet — тик 500 мс и кап 20 c.
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
    engine: Arc<Mutex<Option<T>>>,
    tuning: LocalLoopTuning,
) {
    let actx = crate::app_context::detect();
    let start_fp = actx.target_fingerprint();
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
        std::thread::sleep(Duration::from_millis(tick_ms));
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
            // В режиме «только плашка» это не поле ввода, а последний текст,
            // который пользователь реально видел. Финал использует его как
            // страховку против редких ASR-галлюцинаций/старого хвоста.
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

        if audio::write_wav_16k_mono(&wav, &trimmed).is_err() {
            continue;
        }

        // Берём asr-замок неблокирующе: если идёт финал/другая операция — пропускаем тик.
        let txt = {
            let Some(_g) = a.asr_lock.try_lock() else {
                continue;
            };
            match asr::transcribe_server_partial(a.port, &wav, &a.language) {
                Ok(t) => t,
                Err(_) => continue, // тик глотает ошибку — частичные результаты best-effort
            }
        };

        if txt.trim().is_empty() {
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
    // Поколение ЭТОЙ диктовки — финал-поток сверит его перед вставкой (C4).
    let my_gen = ctx.gen.load(Ordering::SeqCst);
    // UX: как только пользователь завершил запись, оверлей должен сразу уйти из
    // текстовой плашки в AquaVoice-style spinner, пока мы останавливаем partial-loop
    // и готовим финальный ASR.
    set_status(&ctx.app, "transcribing");

    // Останавливаем петлю частичных результатов. Ждём её НЕ дольше ~150 мс
    // (P2-5): ASR-тик может длиться до ~1 c (whisper), и безусловный join
    // подвешивал отпускание клавиши. Не успела — детачим: петля гаснет сама по
    // stop-флагу, а пересечение с финалом исключают мьютексы движков/asr_lock
    // (тик берёт их try_lock'ом и при занятости пропускается).
    let pstate = ctx.partial.lock().take();
    if let Some(mut st) = pstate {
        st.stop.store(true, Ordering::Release);
        if let Some(j) = st.join.take() {
            let t0 = Instant::now();
            loop {
                if j.is_finished() {
                    let _ = j.join();
                    break;
                }
                if t0.elapsed() >= Duration::from_millis(150) {
                    dbg_log("stop: петля партиалов не успела за 150 мс — детачим (P2-5)");
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        // Переносим живое состояние в финальный проход (для inject_incremental реконсиляции).
        let live = LiveState {
            stream_mode: st.stream_mode,
            injected: st.injected,
            committed: st.committed,
            abort: st.abort,
            start_fp: st.start_fp,
        };
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
                set_status(&ctx2.app, "idle");
            }
        });
        return;
    }

    if ctx.settings.lock().play_sounds {
        sound::play(false);
    }
    // Тяжёлую обработку выносим в отдельный поток, чтобы движок мог принять новую запись.
    let ctx2 = ctx.clone();
    let actx = crate::app_context::detect();
    let target_fp = actx.target_fingerprint();
    std::thread::spawn(move || {
        if let Err(err) = process_utterance(&ctx2, samples, rate, None, my_gen, target_fp) {
            log::error!("process_utterance: {err:#}");
            report_process_err(&ctx2.app, &err);
        }
        if ctx2.gen.load(Ordering::SeqCst) == my_gen {
            set_status(&ctx2.app, "idle");
        }
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

fn cancel_current(capture: &mut Option<Capture>, ctx: &EngineCtx) {
    let my_gen = ctx.gen.load(Ordering::SeqCst);
    if let Some(c) = capture.take() {
        let _ = c.finish();
        ctx.recording.store(false, Ordering::SeqCst);
        restore_auto_mute(ctx);
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
        ctx.gen.fetch_add(1, Ordering::SeqCst);
        set_status(&ctx.app, "idle");
        dbg_log("cancel: активная диктовка отменена Esc");
        return;
    }
    ctx.gen.fetch_add(1, Ordering::SeqCst);
    if ctx.improve_busy.load(Ordering::SeqCst) {
        emit_improve_status(&ctx.app, "cancelled", "Отменено");
    }
    set_status(&ctx.app, "idle");
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
    let ctx2 = ctx.clone();
    let my_gen = ctx.gen.load(Ordering::SeqCst);
    std::thread::spawn(move || {
        let result = improve_selection_inner(&ctx2, my_gen);
        ctx2.improve_busy.store(false, Ordering::SeqCst);
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

    let _inj = ctx.inject_lock.lock();
    if ctx.gen.load(Ordering::SeqCst) != my_gen {
        return Ok(());
    }
    let target_fp = actx.target_fingerprint();
    let cur = crate::app_context::detect();
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
struct LiveState {
    stream_mode: String,
    injected: Arc<Mutex<String>>,
    committed: Arc<Mutex<String>>,
    abort: Arc<AtomicBool>,
    start_fp: TargetFingerprint,
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

    fn visible_preview(&self) -> String {
        self.committed.lock().clone()
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
    const PAD: usize = 4800;
    const JOIN_SILENCE: usize = 3200; // 200 мс между склеенными фразами

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
        let start = (start_chunk * chunk).saturating_sub(PAD);
        let end = (((last_voiced + 1) * chunk) + PAD).min(samples.len());
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

    let mut out = Vec::with_capacity(kept + JOIN_SILENCE.saturating_mul(spans.len()));
    for (idx, (start, end)) in spans.into_iter().enumerate() {
        if idx > 0 {
            out.extend(std::iter::repeat_n(0.0, JOIN_SILENCE));
        }
        out.extend_from_slice(&samples[start..end]);
    }
    out
}

fn reconcile_final_with_live_preview(live: Option<&LiveState>, final_text: &str) -> String {
    let Some(live) = live else {
        return final_text.to_string();
    };
    if live.stream_mode != "never" {
        return final_text.to_string();
    }
    let preview = live.visible_preview();
    if looks_like_stale_final(&preview, final_text) {
        dbg_log(&format!(
            "финал: текст почти не совпал с live-preview (preview_len={}, final_len={}) — берём то, что видел пользователь",
            preview.chars().count(),
            final_text.chars().count()
        ));
        return preview;
    }
    final_text.to_string()
}

fn looks_like_stale_final(preview: &str, final_text: &str) -> bool {
    let preview = preview.trim();
    let final_text = final_text.trim();
    if preview.is_empty() || final_text.is_empty() {
        return false;
    }

    let p_tokens = lexical_tokens(preview);
    let f_tokens = lexical_tokens(final_text);
    if p_tokens.len() < 3 || f_tokens.len() < 4 {
        return false;
    }

    let overlap = p_tokens.intersection(&f_tokens).count();
    let overlap_ratio = overlap as f32 / p_tokens.len().min(f_tokens.len()) as f32;
    let p_chars = preview.chars().count();
    let f_chars = final_text.chars().count();
    let final_has_big_tail = f_chars > p_chars + 40 || f_tokens.len() > p_tokens.len() + 8;

    final_has_big_tail && overlap_ratio < 0.25
}

fn lexical_tokens(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter_map(|t| {
            let t = t.trim();
            if t.chars().count() < 2 {
                None
            } else {
                Some(t.to_lowercase())
            }
        })
        .collect()
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
    let _inj = ctx.inject_lock.lock();
    if ctx.gen.load(Ordering::SeqCst) != my_gen {
        dbg_log("отмена: поколение устарело — черновик не трогаем");
        return;
    }
    if l.abort.load(Ordering::Acquire) {
        dbg_log("отмена: живая вставка была прервана — черновик не трогаем");
        return;
    }
    let cur = crate::app_context::detect();
    if !l.start_fp.matches(&cur) {
        dbg_log("отмена: окно сменилось — чужое поле не трогаем");
        return;
    }
    // prev = что физически в поле (injected для always, committed для auto).
    let prev_arc = if l.stream_mode == "always" {
        &l.injected
    } else {
        &l.committed
    };
    let prev = prev_arc.lock().clone();
    if prev.is_empty() {
        return;
    }
    match inject::inject_incremental(&prev, "") {
        Ok(()) => *prev_arc.lock() = String::new(),
        Err(e) => log::warn!("отмена: стирание черновика не удалось: {e}"),
    }
}

fn process_utterance(
    ctx: &EngineCtx,
    samples: Vec<f32>,
    rate: u32,
    live: Option<LiveState>,
    my_gen: u64,
    target_fp: TargetFingerprint,
) -> anyhow::Result<()> {
    if samples.is_empty() {
        return Ok(());
    }
    let s = ctx.settings.lock().clone();
    // Что-то уже физически напечатано клавишами (always/auto) за эту диктовку.
    let live_inserted = live.as_ref().map(|l| l.live_inserted()).unwrap_or(false);

    let t_all = Instant::now();
    let t_pre = Instant::now();
    let mono16 = audio::resample_to_16k(&samples, rate);
    let trimmed = audio::trim_silence(&mono16, 16000);
    let speech_trimmed = compact_speech_for_final_asr(&ctx.vad_final, &trimmed);
    let pre_ms = t_pre.elapsed().as_millis() as u64;
    if speech_trimmed.len() < 16000 / 5 {
        // < ~0.2 c полезного звука — считаем, что речи не было
        return Ok(());
    }

    // Уникальное имя WAV на диктовку (C4): исключает гонку на общем файле, когда
    // финал предыдущей диктовки ещё в полёте, а уже стартовала следующая.
    let wav = paths::unique_tmp_path(&format!("utterance_{my_gen}"), "wav");
    let _wav_guard = paths::TempFileGuard::new(wav.clone());
    audio::write_wav_16k_mono(&wav, &speech_trimmed)?;

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
    let asr_actx = crate::app_context::detect();
    if !target_fp.matches(&asr_actx) {
        dbg_log("финал: окно изменилось до ASR — распознавание отменено");
        erase_live_draft(ctx, live.as_ref(), my_gen);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    let asr_tone =
        crate::app_context::category_for(&asr_actx.exe, &asr_actx.title, &s.app_profile_overrides);
    let asr_prompt = if use_cloud_stt {
        let asr_previous_context_tail = last_dictation_context(ctx, &asr_actx);
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
                    log::warn!("облачный STT недоступен — откат на локальное распознавание");
                    emit_stt_mode(&ctx.app, "local", true);
                    let _g = ctx.asr_lock.lock();
                    local_transcribe(ctx, &s, &dict, &wav)?
                } else {
                    // Fallback выключен — честно сообщаем об ошибке и выходим.
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
                log::warn!("облачный ASR (Gemini) ошибка: {e}; откат на локальное распознавание");
                let _g = ctx.asr_lock.lock();
                local_transcribe(ctx, &s, &dict, &wav)?
            }
        }
    } else {
        let (t, lang) = local_asr(ctx, &s, &dict, &wav, &trimmed)?;
        lang_badge = lang;
        t
    };
    let ms = t0.elapsed().as_millis() as u64;
    // Бейдж языка в пилюле: статус-объект { status, lang } по контракту overlay
    // (legacy-строки "idle"/"recording"/"transcribing" остаются как были).
    // Только если эта диктовка ещё актуальна — не перетираем статус следующей.
    if let Some(l) = lang_badge {
        if ctx.gen.load(Ordering::SeqCst) == my_gen {
            let _ = ctx.app.emit(
                "status",
                serde_json::json!({ "status": "transcribing", "lang": l }),
            );
        }
    }
    let raw = postprocess::dedup_repeated_ngrams(&raw);

    if raw.trim().is_empty() {
        // Гейт уверенности/VAD отклонил (невнятно / тишина / чужой язык).
        // Если в режиме always/auto мы УЖЕ напечатали лучший partial — НЕ стираем
        // экран (никакого mass-backspace), оставляем как есть; иначе старое поведение.
        //
        // ВАЖНО: это не системная ошибка. Пользователь мог случайно тапнуть хоткей,
        // отпустить слишком рано или говорить тише VAD-порога. Раньше здесь играл
        // sound::fail(), из-за чего нормальные "ничего не распознано" ощущались как
        // поломка приложения. Реальные ошибки (нет модели, облако недоступно и т.п.)
        // по-прежнему идут через emit_error/no_model и отдельные fail-пути.
        if live_inserted {
            dbg_log("финал отклонён, но живой текст уже вставлен — не стираем");
        }
        let _ = ctx.app.emit(
            "norecog",
            serde_json::json!({ "message": "Не расслышал — повторите чётче" }),
        );
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // Контекст окна нужен и для тона, и для payload Ollama, и для rule-based
    // продолжения фразы. Детектим один раз после ASR и до постобработки.
    let actx = crate::app_context::detect();
    if !target_fp.matches(&actx) {
        dbg_log("финал: окно изменилось до постобработки — вставка отменена");
        erase_live_draft(ctx, live.as_ref(), my_gen);
        return Ok(());
    }
    dbg_log(&format!(
        "app: exe={} title_len={} → {}",
        actx.exe,
        actx.title.chars().count(),
        actx.category
    ));
    // Тон по приложению считаем через category_for — он учитывает пользовательские
    // app_profile_overrides (ветка B) ПЕРЕД встроенной таблицей классификации.
    let tone = crate::app_context::category_for(&actx.exe, &actx.title, &s.app_profile_overrides);
    let previous_context_tail = last_dictation_context(ctx, &actx);

    // Постобработка (правила) + выученные исправления.
    let t_post = Instant::now();
    let mut text = postprocess::process(&raw, &s, &dict, &snippets);
    text = postprocess::apply_corrections(&text, &corrections);
    let post_ms = t_post.elapsed().as_millis() as u64;
    if text.trim().is_empty() {
        // Постобработка съела весь текст — экран не трогаем (как и при отклонении гейта).
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // Голосовые команды оставляем как совместимость, но снимаем их ДО LLM:
    // форматирование должно быть автоматическим, а модель не должна съедать хвостовое
    // "отмена"/"абзац" до того, как движок успел распознать команду.
    text = postprocess::normalize_spaces(&text);
    text = match crate::voice_cmds::apply_voice_commands(&text) {
        crate::voice_cmds::CmdOutcome::Cancel => {
            dbg_log("финал: голосовая команда «отмена» — вставка и история пропущены");
            erase_live_draft(ctx, live.as_ref(), my_gen);
            let _ = std::fs::remove_file(&wav);
            return Ok(());
        }
        crate::voice_cmds::CmdOutcome::Text(t) => t,
    };
    if text.is_empty() {
        erase_live_draft(ctx, live.as_ref(), my_gen);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    text = reconcile_final_with_live_preview(live.as_ref(), &text);

    // ── «Умный» рерайт под стиль активного приложения (Gemini/Ollama/OpenAI-compatible) ──
    // verbatim и нейтральный/пустой профиль LLM не зовут (правила уже отработали).
    // "ai" теперь eligible: пользователь ожидает Wispr Flow-поведение — из одной
    // сбивчивой диктовки получить готовый промпт. Если LLM недоступна, остаётся
    // быстрый deterministic fallback после правил.
    let (smart_instruction, ai_prompt_context) =
        effective_smart_instruction_for_app(&s, &actx, &tone);
    let context_hint = rewrite_context_hint(ctx, &actx, None);
    let rewrite_tone = if ai_prompt_context {
        "ai"
    } else {
        tone.as_str()
    };
    let smart_active = smart_instruction.is_some();
    let llm_eligible = !s.verbatim
        && (ai_prompt_context
            || smart_active
            || (!rewrite_tone.is_empty()
                && rewrite_tone != "neutral"
                && rewrite_tone != "verbatim"
                && rewrite_tone != "code"));
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
    text = postprocess::normalize_spaces(&text);
    if conversational_continuation_enabled(&tone) {
        text = postprocess::soften_false_sentence_breaks(&text);
    }
    text = continue_from_previous_context(&text, previous_context_tail.as_deref(), &tone);
    text = postprocess::normalize_spaces(&text);
    if text.trim().is_empty() {
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
    // always/auto (петля была): реконсиляция уже напечатанного → финальный текст
    //   КЛАВИШАМИ (inject_incremental) — предыдущее тоже печаталось, диффы валидны.
    //   При смене окна (abort) чужое поле не трогаем.
    // never / без петли: обычная вставка целиком (clipboard/type как раньше).
    let live_mode = live
        .as_ref()
        .map(|l| l.stream_mode.as_str())
        .unwrap_or("never");
    // Замок эмиссии клавиш на всю финальную вставку — чтобы нажатия этой диктовки не
    // пересеклись с тиками/финалом следующей при быстром рестарте. Дропается в конце
    // блока (или раньше — при ? в never-ветке, через RAII).
    let inject_guard = ctx.inject_lock.lock();
    // C4 (TOCTOU): пока ждали inject_lock, могла стартовать НОВАЯ диктовка.
    // Перепроверяем поколение уже ПОД замком — иначе осиротевший поток всё равно
    // вставит устаревший/перекрывающийся текст (остаточная многократная вставка при rapid-fire).
    if ctx.gen.load(Ordering::SeqCst) != my_gen {
        dbg_log("финал: поколение устарело под inject_lock — вставку пропускаем");
        drop(inject_guard);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    // Идемпотентность вставки (пояс поверх gen-guard): одно поколение вставляется РОВНО
    // один раз. Если этот gen уже вставлялся (теоретически — два detached-потока финала
    // совпали по gen), второй проход НЕ дублирует текст в поле. swap атомарно фиксирует
    // «этот gen вставлен» и возвращает прежнее значение.
    if ctx.last_injected_gen.swap(my_gen, Ordering::SeqCst) == my_gen {
        dbg_log("финал: это поколение уже вставлено — пропускаем (идемпотентность)");
        drop(inject_guard);
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }
    let mut final_inserted = false;
    let t_inj = Instant::now();
    match (live.as_ref(), live_mode) {
        (Some(l), "always") | (Some(l), "auto") => {
            let cur = crate::app_context::detect();
            if l.abort.load(Ordering::Acquire) {
                dbg_log("финал: окно сменилось — реконсиляцию пропускаем");
            } else if !l.start_fp.matches(&cur) {
                l.abort.store(true, Ordering::Release);
                dbg_log("финал: целевое окно изменилось — реконсиляцию пропускаем");
            } else {
                // prev = что уже физически в поле (injected для always, committed для auto).
                let prev = if l.stream_mode == "always" {
                    l.injected.lock().clone()
                } else {
                    l.committed.lock().clone()
                };
                if text.contains('\n') {
                    // Абзацы должны быть автоматическими, но Enter в чатах опасен.
                    // Поэтому живой черновик стираем клавишами, а финальный
                    // многострочный текст вставляем одним безопасным clipboard-paste.
                    let _ = inject::set_clipboard_text(&text)
                        .map_err(|e| log::warn!("clipboard final text: {e}"));
                    if let Err(e) = inject::inject_incremental(&prev, "") {
                        log::warn!("финальная очистка live-черновика: {e}");
                    } else if let Err(e) = inject::inject_keep_clipboard(&text, "clipboard") {
                        log::warn!("финальная clipboard-вставка с абзацами: {e}");
                    } else if l.stream_mode == "always" {
                        *l.injected.lock() = text.clone();
                        final_inserted = true;
                    } else {
                        *l.committed.lock() = text.clone();
                        final_inserted = true;
                    }
                } else {
                    let flat = flatten_breaks(&text);
                    let _ = inject::set_clipboard_text(&flat)
                        .map_err(|e| log::warn!("clipboard final text: {e}"));
                    if let Err(e) = inject::inject_incremental(&prev, &flat) {
                        log::warn!("финальная реконсиляция: {e}");
                    } else if l.stream_mode == "always" {
                        *l.injected.lock() = flat;
                        final_inserted = true;
                    } else {
                        *l.committed.lock() = flat;
                        final_inserted = true;
                    }
                }
            }
        }
        _ => {
            // never-режим или петли не было — поведение как раньше (вставка целиком).
            let cur = crate::app_context::detect();
            if !target_fp.matches(&cur) {
                dbg_log("финал: целевое окно изменилось — вставка отменена");
                drop(inject_guard);
                return Ok(());
            }
            // Ошибку пробрасываем ПОСЛЕ уборки временного WAV (иначе утечка в tmp).
            if let Err(e) = inject::inject_keep_clipboard(&text, &s.paste_method) {
                drop(inject_guard);
                let _ = std::fs::remove_file(&wav);
                return Err(e);
            }
            final_inserted = true;
        }
    }
    drop(inject_guard); // освобождаем замок клавиш сразу после вставки
    if final_inserted {
        remember_dictation_context(ctx, &actx, &text);
        emit_final_preview(&ctx.app, &text, my_gen, lang_badge);
    }
    let inject_ms = t_inj.elapsed().as_millis() as u64;
    // Сквозной замер этапов финала: отпускание клавиши → текст в поле.
    dbg_log(&format!(
        "[lat] gen={my_gen} pre={pre_ms}мс asr={ms}мс post={post_ms}мс llm={llm_ms}мс inject={inject_ms}мс total={}мс",
        t_all.elapsed().as_millis()
    ));
    // Запомнить вставленное — для авто-захвата исправлений из буфера (во всех путях).
    *ctx.last_inject.lock() = Some(LastInject {
        text: text.clone(),
        at: Instant::now(),
    });

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

/// Сохранить пару (аудио 16 кГц ↔ распознанный текст) в датасет персонализации.
fn save_sample(ctx: &EngineCtx, wav: &std::path::Path, text: &str) {
    let now = chrono::Local::now();
    let stamp = now.format("%Y%m%d_%H%M%S_%3f").to_string();
    let dest = paths::dataset_dir().join(format!("{stamp}.wav"));
    let audio = match std::fs::copy(wav, &dest) {
        Ok(_) => dest.to_string_lossy().to_string(),
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
    v.sort_by(|a, b| b.1.cmp(&a.1));
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
    let mut guard = ctx.server.lock();
    let need_start = match guard.as_mut() {
        Some(srv) => {
            srv.model.as_path() != model
                || srv.child.try_wait().map(|o| o.is_some()).unwrap_or(true)
        }
        None => true,
    };
    if need_start {
        if let Some(mut old) = guard.take() {
            let _ = old.child.kill();
        }
        let mut last_err: Option<anyhow::Error> = None;
        for _ in 0..5 {
            let port = asr::reserve_loopback_port()?;
            match asr::start_server(whisper_dir, model, port, threads) {
                Ok(srv) => {
                    *guard = Some(srv);
                    return Ok(port);
                }
                Err(e) => last_err = Some(e),
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
    if paths::model_path(&s.model).exists() {
        return true;
    }
    if let Ok(rd) = std::fs::read_dir(paths::models_dir()) {
        for entry in rd.flatten() {
            if entry.path().extension().and_then(|x| x.to_str()) == Some("bin") {
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
        LocalRoute::GigaAm => !crate::gigaam::dir_ready(&paths::gigaam_dir()) && !whisper_ready(),
        LocalRoute::Parakeet => {
            !crate::parakeet::dir_ready(&paths::parakeet_dir()) && !whisper_ready()
        }
        LocalRoute::Whisper => !whisper_ready(),
    }
}

/// Выбрать модель: из настроек, иначе — самая БОЛЬШАЯ установленная *.bin
/// (эвристика «самая мощная»), иначе типизированная ошибка `ModelMissing`.
fn resolve_model(s: &Settings) -> anyhow::Result<std::path::PathBuf> {
    let p = paths::model_path(&s.model);
    if p.exists() {
        return Ok(p);
    }
    let mut best: Option<(u64, std::path::PathBuf)> = None;
    if let Ok(rd) = std::fs::read_dir(paths::models_dir()) {
        for entry in rd.flatten() {
            let pp = entry.path();
            if pp.extension().and_then(|x| x.to_str()) == Some("bin") {
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

fn set_status(app: &AppHandle, status: &str) {
    let _ = app.emit("status", status);
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
/// en/auto → Parakeet при установленной модели, прочие языки → whisper. Общий VAD-гейт тишины для
/// резидентных движков. Возвращает (текст, язык для бейджа overlay).
fn local_asr(
    ctx: &EngineCtx,
    s: &Settings,
    dict: &[postprocess::Dict],
    wav: &std::path::Path,
    samples_16k: &[f32],
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
    // en/auto без Parakeet — текущее поведение (whisper) + разовая дружелюбная
    // подсказка, что с Parakeet будет лучше.
    if route == LocalRoute::Whisper
        && s.engine == "gigaam"
        && (s.language == "en" || s.language == "auto")
    {
        hint_parakeet_once(&ctx.app);
    }
    let whisper_text = {
        let _g = ctx.asr_lock.lock();
        local_transcribe(ctx, s, dict, wav)?
    };
    if s.language == "auto"
        && s.engine == "gigaam"
        && crate::gigaam::dir_ready(&paths::gigaam_dir())
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

/// Разовая (на сессию) подсказка для en/auto без установленного Parakeet:
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
            "message": "Для английского и автоопределения языка установите модель «Parakeet TDT v3» во вкладке «Модель» — точнее и с живыми партиалами"
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
    }

    #[test]
    fn stale_final_guard_rejects_unseen_big_tail() {
        assert!(looks_like_stale_final(
            "Исправьте пожалуйста этот текст",
            "Прошлая длинная фраза вообще из другой диктовки и старого сообщения которое пользователь сейчас не говорил"
        ));
    }

    #[test]
    fn stale_final_guard_accepts_same_dictation_with_more_words() {
        assert!(!looks_like_stale_final(
            "Исправьте пожалуйста этот текст",
            "Исправьте пожалуйста этот текст и сделайте его немного понятнее"
        ));
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
    fn auto_language_uses_parakeet_when_installed_otherwise_whisper() {
        let s = Settings {
            engine: "gigaam".to_string(),
            language: "auto".to_string(),
            ..Settings::default()
        };
        assert_eq!(local_route_with_parakeet(&s, true), LocalRoute::Parakeet);
        assert_eq!(local_route_with_parakeet(&s, false), LocalRoute::Whisper);
    }
}

/// Локальное распознавание whisper (server → cli fallback).
fn local_transcribe(
    ctx: &EngineCtx,
    s: &Settings,
    dict: &[postprocess::Dict],
    wav: &std::path::Path,
) -> anyhow::Result<String> {
    let whisper_dir = paths::whisper_dir(&ctx.app);
    let model = resolve_model(s)?;
    // Для русского даём короткую языковую затравку даже при пустом словаре —
    // это удерживает декодер в русском и улучшает выбор слов. Для en/auto затравки нет.
    let base_prompt = if s.language == "ru" {
        Some(postprocess::DEFAULT_RU_PROMPT)
    } else {
        None
    };
    let prompt = postprocess::dict_bias_prompt(dict, base_prompt);
    let params = AsrParams {
        whisper_dir: &whisper_dir,
        model_path: &model,
        wav_path: wav,
        language: &s.language,
        threads: s.effective_threads(),
        initial_prompt: prompt.as_deref(),
    };
    if s.engine == "whisper_cli" {
        asr::transcribe_cli(&params)
    } else {
        match ensure_server(ctx, &whisper_dir, &model, s.effective_threads())
            .and_then(|port| asr::transcribe_server(port, wav, &s.language, prompt.as_deref()))
        {
            Ok(t) => Ok(t),
            Err(e) => {
                log::warn!("whisper-server недоступен ({e}), откат на cli");
                asr::transcribe_cli(&params)
            }
        }
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
        .map(|(exe, title)| exe == &actx.exe && title == &actx.title)
        .unwrap_or(false)
}

fn last_dictation_context(
    ctx: &EngineCtx,
    actx: &crate::app_context::AppContext,
) -> Option<String> {
    let memory = ctx.dictation_memory.lock();
    if !target_matches_memory(&memory, actx) {
        return None;
    }
    memory.recent.back().cloned()
}

fn conversational_continuation_enabled(tone: &str) -> bool {
    matches!(tone, "" | "neutral" | "casual" | "very_casual" | "work")
}

fn continue_from_previous_context(text: &str, previous: Option<&str>, tone: &str) -> String {
    if !conversational_continuation_enabled(tone) {
        return text.to_string();
    }
    let Some(prev) = previous.map(str::trim).filter(|v| !v.is_empty()) else {
        return text.to_string();
    };
    if previous_context_is_closed(prev) {
        return text.to_string();
    }
    let Some(next) = lower_if_continuation_start(text) else {
        return text.to_string();
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
    text.trim_end()
        .chars()
        .rev()
        .find(|c| !matches!(c, '"' | '\'' | ')' | ']' | '}' | '»' | '”'))
        .map(|c| ".!?…\n".contains(c))
        .unwrap_or(false)
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

fn merge_context_summary(current: &str, old: &str) -> String {
    let merged = if current.trim().is_empty() {
        old.to_string()
    } else {
        format!("{current} {old}")
    };
    compact_context_tail(&merged, DICTATION_CONTEXT_SUMMARY_CHARS)
}

fn recent_context_len(recent: &VecDeque<String>) -> usize {
    recent.iter().map(|v| v.chars().count()).sum()
}

fn remember_dictation_context(ctx: &EngineCtx, actx: &crate::app_context::AppContext, text: &str) {
    let compact = compact_context_tail(text, DICTATION_CONTEXT_ITEM_CHARS);
    if compact.trim().is_empty() {
        return;
    }

    let mut memory = ctx.dictation_memory.lock();
    let target_fp = (actx.exe.clone(), actx.title.clone());
    if memory.target_fp.as_ref() != Some(&target_fp) {
        memory.target_fp = Some(target_fp);
        memory.summary.clear();
        memory.recent.clear();
    }

    memory.recent.push_back(compact);
    while memory.recent.len() > DICTATION_CONTEXT_RECENT_LIMIT
        || recent_context_len(&memory.recent) > DICTATION_CONTEXT_RECENT_CHARS
    {
        let Some(old) = memory.recent.pop_front() else {
            break;
        };
        memory.summary = merge_context_summary(&memory.summary, &old);
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
                    .map(|v| v.as_str())
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
        "Speech recognition context only: transcribe what was said, preserve Russian/English/other language switches, do not rewrite.".to_string(),
    ];

    let app = app_label_for_payload(actx);
    if !app.trim().is_empty() {
        if tone.trim().is_empty() || tone == "neutral" {
            parts.push(format!("Active app: {app}."));
        } else {
            parts.push(format!("Active app: {app}; style context: {tone}."));
        }
    }

    if let Some(previous) = previous_context_tail
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        parts.push(format!(
            "Previous same-field text tail: {}.",
            compact_context_tail(previous, ASR_PROMPT_PREVIOUS_CHARS)
        ));
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

    let mut attempts: Vec<Box<dyn Fn() -> anyhow::Result<String>>> = Vec::new();
    if s.ai_backend == "gemini" && crate::gemini::available(&s.ai_api_key) {
        let key = s.ai_api_key.clone();
        let model = s.ai_model.clone();
        let instruction =
            build_tone_instruction(target_tone, smart_instruction, context_hint, corrections);
        let input = text.to_string();
        attempts.push(Box::new(move || {
            crate::gemini::refine(&key, &model, &instruction, &input)
        }));
    }
    if s.ai_backend == "openai_compat" && crate::rewrite::configured(s) {
        let settings = s.clone();
        let user =
            build_voiceflow_payload(actx, text, target_tone, smart_instruction, context_hint);
        attempts.push(Box::new(move || {
            crate::rewrite::refine(&settings, crate::ollama::SYSTEM_PROMPT, &user)
        }));
    }
    if s.ai_backend != "off" && crate::ollama::configured(&s.ollama_url) {
        let url = s.ollama_url.clone();
        let model = s.ollama_model.clone();
        let user =
            build_voiceflow_payload(actx, text, target_tone, smart_instruction, context_hint);
        attempts.push(Box::new(move || {
            crate::ollama::refine(&url, &model, crate::ollama::SYSTEM_PROMPT, &user)
        }));
    }

    for attempt in attempts {
        match attempt() {
            Ok(r) if !r.trim().is_empty() => {
                return (postprocess::normalize_spaces(r.trim()), true)
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
    fn gemini_instruction_includes_context_guardrail() {
        let instruction = build_tone_instruction(
            "doc",
            None,
            Some("Краткая выжимка более ранней диктовки: обсуждали релиз"),
            &[],
        );

        assert!(instruction.contains("Контекст для понимания соседнего текста"));
        assert!(instruction.contains("не добавляй новые факты из контекста"));
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
        };

        let (instruction, is_ai) = effective_smart_instruction_for_app(&s, &actx, "work");

        assert!(is_ai);
        assert!(instruction
            .expect("ai default instruction")
            .contains("готовый промпт"));
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
        assert!(prompt.contains("Previous same-field text tail"));
        assert!(prompt.contains("Wispr Flow"));
        assert!(prompt.contains("VoxFlow"));
        assert!(prompt.contains("sig"));
        assert!(prompt.contains("Виспа Фолл -> Wispr Flow"));
        assert!(!prompt.contains("super secret expanded template"));
        assert!(prompt.chars().count() <= ASR_PROMPT_MAX_CHARS);
    }
}

/// Монитор буфера обмена: если пользователь скопировал отредактированную версию
/// последней вставки — выучить пословные исправления (распознано → правильно).
/// Тикает ТОЛЬКО при включённой персонализации (P2-11): обучение выключено →
/// буфер обмена вообще не опрашивается (ни лишнего CPU, ни чтения чужих копий).
fn clipboard_monitor(ctx: EngineCtx) {
    // Базовый снимок берём лениво — первый тик после включения персонализации
    // лишь запоминает текущее содержимое, ничего не «выучивая» задним числом.
    let mut last_seen: Option<String> = None;
    loop {
        std::thread::sleep(Duration::from_millis(1300));
        if !ctx.settings.lock().personalize {
            last_seen = None; // после повторного включения — свежий базовый снимок
            std::thread::sleep(Duration::from_millis(1700)); // редкая проверка тоггла (~3 c)
            continue;
        }
        // Пока инжектор печатает/работает с буфером — не лезем в clipboard
        // (contention с arboard внутри вставки = подвисания и порча восстановления).
        if inject::is_busy() {
            continue;
        }
        let cur = match arboard::Clipboard::new()
            .ok()
            .and_then(|mut c| c.get_text().ok())
        {
            Some(t) => t,
            None => continue,
        };
        let Some(prev) = last_seen.replace(cur.clone()) else {
            continue; // первый снимок после включения — только базовая точка
        };
        if cur == prev || cur.trim().is_empty() {
            continue;
        }
        let injected = ctx.last_inject.lock().clone();
        if let Some(inj) = injected {
            if cur.trim() == inj.text.trim() {
                continue; // это наш же текст — не учим
            }
            try_learn(&ctx, &inj, &cur);
        }
    }
}

/// Выучить исправления из пары (вставлено → отредактировано пользователем).
fn try_learn(ctx: &EngineCtx, injected: &LastInject, edited: &str) {
    if injected.at.elapsed() > Duration::from_secs(10 * 60) {
        return;
    }
    let pairs = learned_correction_pairs(&injected.text, edited, injected.at.elapsed());
    if pairs.is_empty() {
        return; // не похоже на правку (или идентично)
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

#[derive(Clone, Debug)]
struct LearnToken {
    raw: String,
    norm: String,
}

fn learned_correction_pairs(injected: &str, edited: &str, age: Duration) -> Vec<(String, String)> {
    let wrong = learning_tokens(injected);
    let right = learning_tokens(edited);
    if wrong.is_empty() || right.is_empty() || token_norms_equal(&wrong, &right) {
        return Vec::new();
    }
    if wrong.len() > 80 || right.len() > 80 {
        return Vec::new();
    }

    let anchors = lcs_token_anchors(&wrong, &right);
    let mut out = Vec::new();
    if anchors.is_empty() {
        push_learned_span(&mut out, &wrong, &right, false, age);
        return dedup_learned_pairs(out);
    }

    let mut prev_w = 0usize;
    let mut prev_r = 0usize;
    for (wi, ri) in anchors
        .into_iter()
        .chain(std::iter::once((wrong.len(), right.len())))
    {
        push_learned_span(&mut out, &wrong[prev_w..wi], &right[prev_r..ri], true, age);
        prev_w = wi.saturating_add(1);
        prev_r = ri.saturating_add(1);
    }
    dedup_learned_pairs(out)
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

fn push_learned_span(
    out: &mut Vec<(String, String)>,
    wrong: &[LearnToken],
    right: &[LearnToken],
    anchored: bool,
    age: Duration,
) {
    if wrong.is_empty() || right.is_empty() {
        return;
    }
    if span_pair_valid(wrong, right, anchored, age) {
        out.push((join_learn_tokens(wrong), join_learn_tokens(right)));
    }
    if wrong.len() == right.len() {
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
        return true;
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
    fn rejects_unrelated_copied_text() {
        let pairs = learned_correction_pairs("привет", "password", Duration::from_secs(15));

        assert!(pairs.is_empty());
    }

    #[test]
    fn rejects_stale_direct_clipboard_change_without_anchors() {
        let pairs = learned_correction_pairs("Виспа Фолл", "Wispr Flow", Duration::from_secs(240));

        assert!(pairs.is_empty());
    }
}
