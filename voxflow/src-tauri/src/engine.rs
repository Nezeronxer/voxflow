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

use crate::asr::{self, AsrParams};
use crate::audio::{self, Capture};
use crate::settings::Settings;
use crate::{db, inject, paths, postprocess};
use std::collections::{HashMap, HashSet};

/// Звуки старт/стоп (Windows: MessageBeep, без зависимостей; неблокирующий).
#[cfg(windows)]
mod sound {
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
    pub fn fail() {}
}

/// Команды движку (из хоткея, трея, UI).
pub enum EngineCmd {
    Start,
    Stop,
    Toggle,
    Shutdown,
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
    /// (exe,title) активного окна на старте — отпечаток целевого поля.
    start_fp: (String, String),
    /// Режим вставки на момент старта: "never" | "auto" | "always".
    stream_mode: String,
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
    last_inject: Arc<Mutex<Option<String>>>,
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
    /// Резидентный Silero VAD для ПАРТИАЛ-петли (несёт стриминговый state
    /// поверх тиков — его нельзя сбрасывать чужими вызовами).
    vad: Arc<Mutex<Option<crate::vad::SileroVad>>>,
    /// Отдельный VAD для ФИНАЛОВ (has_speech-гейт, сегментация длинного аудио).
    /// Финал — detached-поток и при быстром рестарте перекрывается с петлёй
    /// СЛЕДУЮЩЕЙ диктовки; общий инстанс ломал бы её стриминговый state.
    vad_final: Arc<Mutex<Option<crate::vad::SileroVad>>>,
}

/// Поднять рабочий поток движка.
pub fn spawn(
    app: AppHandle,
    rx: Receiver<EngineCmd>,
    db: Arc<Mutex<Connection>>,
    settings: Arc<Mutex<Settings>>,
    recording: Arc<AtomicBool>,
) {
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
        vad: Arc::new(Mutex::new(None)),
        vad_final: Arc::new(Mutex::new(None)),
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
}

/// Простой файловый лог для диагностики (data_dir/debug.log).
pub fn dbg_log(msg: &str) {
    use std::io::Write;
    let p = paths::data_dir().join("debug.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
        let now = chrono::Local::now().format("%H:%M:%S%.3f");
        let _ = writeln!(f, "[{now}] {msg}");
    }
}

/// Заранее поднять и прогреть резидентные модели (GigaAM/VAD или whisper-server),
/// чтобы первая диктовка не ждала загрузку/JIT.
fn warmup(ctx: EngineCtx) {
    std::thread::sleep(Duration::from_millis(1200));
    let s = ctx.settings.lock().clone();
    dbg_log(&format!("warmup: engine={}, model={}", s.engine, s.model));
    // VAD грузим всегда (2 МБ, мгновенно) — гейт тишины нужен во всех режимах.
    // Два инстанса: петля партиалов (стриминговый state) и финалы — раздельно.
    {
        let t = Instant::now();
        let p = paths::vad_model_path(Some(&ctx.app));
        match (crate::vad::SileroVad::load(&p), crate::vad::SileroVad::load(&p)) {
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
    if s.engine == "gigaam" {
        // Основной путь: резидентный GigaAM. whisper-server не поднимаем —
        // он нужен только для en/фолбэка и стартует лениво.
        match ensure_gigaam(&ctx, &s) {
            Ok(()) => {
                if let Some(g) = ctx.gigaam.lock().as_mut() {
                    let t = Instant::now();
                    let _ = g.transcribe(&vec![0.0f32; 8000]);
                    dbg_log(&format!("warmup: gigaam прогрет за {} мс", t.elapsed().as_millis()));
                }
            }
            Err(e) => dbg_log(&format!("warmup: gigaam ОШИБКА: {e:#} (модель скачается при первом запуске)")),
        }
        return;
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
            EngineCmd::Shutdown => {
                if let Some(mut srv) = ctx.server.lock().take() {
                    let _ = srv.child.kill();
                }
                break;
            }
        }
    }
}

fn start_capture_into(capture: &mut Option<Capture>, ctx: &EngineCtx) {
    if capture.is_some() {
        return;
    }
    let (device, play) = {
        let s = ctx.settings.lock();
        (s.input_device.clone(), s.play_sounds)
    };
    // B3: для локального whisper модель обязательна — без неё НЕ начинаем «запись в
    // никуда», а сразу показываем предупреждение «Выберите модель». Облачный ASR
    // (Gemini-транскрипция ИЛИ облачный STT-провайдер) модель не требует, поэтому
    // проверяем только для чисто локального пути.
    {
        let s = ctx.settings.lock();
        // Облако «активно» только при наличии ключа — иначе провайдер openai_compat/deepgram
        // de-facto уходит в локальный whisper (умный фолбэк). Зеркалим ту же проверку, что
        // и в process_utterance: без ключа провайдер облачный, но модель нам ВСЁ РАВНО нужна,
        // иначе гард «выберите модель» пропустили бы и юзер записал бы «в никуда» (баг старта).
        let cloud_key_ok = match s.stt_provider.as_str() {
            "openai_compat" => !s.resolve_oai_key().is_empty(),
            "deepgram" => !s.resolve_deepgram_key().is_empty(),
            _ => false,
        };
        let use_cloud_stt =
            (s.stt_provider == "openai_compat" || s.stt_provider == "deepgram") && cloud_key_ok;
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
    // крутим локальный whisper-server, чтобы показать МГНОВЕННЫЙ серый ЧЕРНОВИК в пилюле;
    // в поле при этом ничего не печатаем (effective_mode → "never" ниже), потому что точный
    // финал придёт из облака и вставится один раз. Если ключа нет — мы de-facto работаем
    // локально, поведение как у "local" (умный фолбэк, решение пользователя).
    let cloud_active = match s.stt_provider.as_str() {
        "openai_compat" => !s.resolve_oai_key().is_empty(),
        "deepgram" => !s.resolve_deepgram_key().is_empty(),
        _ => false,
    };
    // ОБЛАЧНЫЙ живой черновик: если STT — облако с ключом, локальный whisper/GPU/модель
    // НЕ нужны. Шлём растущий буфер прямо в облако (Groq/Avalon/Deepgram) каждые ~1.4с →
    // серый текст в пилюле, «как у офлайн-моделей», но через API-ключ. В поле НЕ печатаем
    // (точный финал вставится один раз). Это и УБИРАЕТ ложный наг «выберите модель»:
    // раньше гибрид пытался поднять локальный whisper ради черновика и при отсутствии
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
    // GigaAM: живые партиалы на CPU, GPU не нужен. Сегментная схема: VAD находит
    // паузы; завершённые сегменты фиксируются (committed растёт монотонно по
    // построению), активный сегмент перераспознаётся каждый тик (volatile, серый).
    // По тишине ASR не гоняем вовсе.
    if s.engine == "gigaam" && s.language == "ru" {
        if ensure_gigaam(ctx, &s).is_err() {
            // Модели ещё нет (первый запуск, докачка) — пилюля статична,
            // предупреждение по-старому отработает финал/гард старта.
            dbg_log("partial: gigaam не готов — без живого стрима");
            return;
        }
        start_gigaam_partial_loop(capture, ctx, &s);
        return;
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
            dbg_log(&format!("partial: resolve_model ошибка: {e} — без стриминга"));
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
            dbg_log(&format!("partial: ensure_server ошибка: {e:#} — без стриминга"));
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
    let start_fp = (actx.exe, actx.title);
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
    let start_fp = (actx.exe, actx.title);
    let my_seq = ctx.gen.load(Ordering::SeqCst);

    let stop = Arc::new(AtomicBool::new(false));
    let rate = capture.sample_rate();
    let buf = capture.buffer_handle();
    let app = ctx.app.clone();
    let s_clone = s.clone();
    let t_stop = Arc::clone(&stop);

    // Детачим (handle не сохраняем) — stop_and_process не будет ждать сетевой запрос.
    let _ = std::thread::Builder::new()
        .name("voxflow-cloud-partial".into())
        .spawn(move || cloud_partial_loop(buf, rate, app, s_clone, t_stop, my_seq));

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
fn cloud_partial_loop(
    buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    rate: u32,
    app: AppHandle,
    s: Settings,
    stop: Arc<AtomicBool>,
    seq: u64,
) {
    let min_new = rate as usize; // ≥1с НОВОГО звука на тик (экономим запросы)
    let mut last_len = 0usize;
    let mut sent = 0u32;
    let mut idle = 0u32; // тиков подряд без нового звука (тишина/пауза)
    let mut stab = PrefixStabilizer::new(4, 2);
    let wav = paths::tmp_dir().join(format!("cloud_partial_{seq}.wav"));
    loop {
        std::thread::sleep(Duration::from_millis(2000)); // коарс-каденс (бюджет API)
        if stop.load(Ordering::Acquire) || sent >= CLOUD_DRAFT_CAP {
            break;
        }
        // Снимок буфера: нужно ≥1с нового звука с прошлого УСПЕШНОГО тика.
        let snapshot: Vec<f32> = match buffer.lock() {
            Ok(g) => {
                if g.len() < last_len + min_new {
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
                g.clone()
            }
            Err(_) => continue,
        };
        let mono16 = audio::resample_to_16k(&snapshot, rate);
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
        last_len = snapshot.len();
        sent += 1;
        if text.trim().is_empty() {
            continue;
        }
        let (committed, volatile) = stab.push(&text);
        let full = match (committed.is_empty(), volatile.is_empty()) {
            (false, false) => format!("{committed} {volatile}"),
            (false, true) => committed.clone(),
            (true, false) => volatile.clone(),
            (true, true) => String::new(),
        };
        // Третья проверка stop — прямо перед эмиссией: закрываем узкое TOCTOU-окно,
        // чтобы НЕ показать черновик ПОВЕРХ уже идущего финала (отпустили клавишу).
        if stop.load(Ordering::Acquire) {
            break;
        }
        let _ = app.emit(
            "partial",
            serde_json::json!({ "text": full, "committed": committed, "volatile": volatile, "seq": seq }),
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

/// Похоже ли `b` на переговорённую заново версию `a` (человек ошибся, сделал
/// паузу и сказал фразу ещё раз): пословный Жаккар >= 0.5 при сопоставимой
/// длине. Сравниваем только СОСЕДНИЕ сегменты — типичный паттерн самоправки.
fn is_restatement(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> Vec<String> {
        s.split_whitespace()
            .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
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
    let seq = ctx.gen.load(Ordering::SeqCst);
    let _ = std::thread::Builder::new().name("voxflow-level".into()).spawn(move || {
        let win = (rate / 20).max(1); // окно RMS ~50 мс
        while recording.load(Ordering::SeqCst) {
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

/// Аргументы петли живых партиалов GigaAM (CPU, сегментная схема по VAD-паузам).
struct GigaamLoopArgs {
    buffer: Arc<std::sync::Mutex<Vec<f32>>>,
    rate: u32,
    app: AppHandle,
    gigaam: Arc<Mutex<Option<crate::gigaam::GigaAm>>>,
    vad: Arc<Mutex<Option<crate::vad::SileroVad>>>,
    inject_lock: Arc<Mutex<()>>,
    stop: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
    injected: Arc<Mutex<String>>,
    committed_field: Arc<Mutex<String>>,
    start_fp: (String, String),
    stream_mode: String,
    seq: u64,
}

/// Поднять петлю живых партиалов GigaAM. stream_mode действует как у whisper:
/// never — только пилюля, auto — committed в поле, always — всё в поле.
fn start_gigaam_partial_loop(capture: &Capture, ctx: &EngineCtx, s: &Settings) {
    let actx = crate::app_context::detect();
    let start_fp = (actx.exe, actx.title);
    let my_seq = ctx.gen.load(Ordering::SeqCst);

    let stop = Arc::new(AtomicBool::new(false));
    let abort = Arc::new(AtomicBool::new(false));
    let injected = Arc::new(Mutex::new(String::new()));
    let committed = Arc::new(Mutex::new(String::new()));

    let args = GigaamLoopArgs {
        buffer: capture.buffer_handle(),
        rate: capture.sample_rate(),
        app: ctx.app.clone(),
        gigaam: Arc::clone(&ctx.gigaam),
        vad: Arc::clone(&ctx.vad),
        inject_lock: Arc::clone(&ctx.inject_lock),
        stop: Arc::clone(&stop),
        abort: Arc::clone(&abort),
        injected: Arc::clone(&injected),
        committed_field: Arc::clone(&committed),
        start_fp: start_fp.clone(),
        stream_mode: s.stream_mode.clone(),
        seq: my_seq,
    };
    let join = std::thread::Builder::new()
        .name("voxflow-gigaam-partial".into())
        .spawn(move || gigaam_partial_loop(args))
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

/// Тело петли GigaAM-партиалов. VAD стримово размечает новые сэмплы; пауза
/// ≥600 мс (или сегмент ≥25 c — лимит модели) закрывает активный сегмент:
/// его текст один раз фиксируется в committed (больше НЕ переписывается),
/// дальше распознаётся только новый активный кусок. Тишину не распознаём.
fn gigaam_partial_loop(a: GigaamLoopArgs) {
    const TICK_MS: u64 = 350;
    const SPEECH_PROB: f32 = 0.35;
    const SIL_BOUND_MS: usize = 600;
    const MAX_SEG_SAMPLES: usize = 25 * 16000;

    // Пауза >=2 c между фразами = новый абзац в финальном тексте.
    const PARA_GAP_SAMPLES: usize = 2 * 16000;
    let mut committed_segs: Vec<(bool, String)> = Vec::new();
    let mut seg_start = 0usize; // оффсет активного сегмента (16к-домен)
    let mut vad_pos = 0usize; // докуда прогнали стриминговый VAD
    let mut last_speech_end = 0usize; // конец последнего речевого VAD-чанка
    let mut prev_seg_end = 0usize; // конец речи последнего ЗАКРЫТОГО сегмента
    let mut cur_seg_first_speech: Option<usize> = None;
    let mut seg_has_speech = false;
    let mut last_emitted: Option<(String, String)> = None;

    // Свой стриминговый VAD-state на диктовку.
    if let Some(v) = a.vad.lock().as_mut() {
        v.reset();
    }

    loop {
        std::thread::sleep(Duration::from_millis(TICK_MS));
        if a.stop.load(Ordering::Acquire) {
            break;
        }
        let snapshot: Vec<f32> = match a.buffer.lock() {
            Ok(g) => g.clone(),
            Err(_) => continue,
        };
        let mono16 = audio::resample_to_16k(&snapshot, a.rate);
        if mono16.len() < vad_pos + crate::vad::CHUNK {
            continue;
        }
        // Стриминговый VAD только по НОВЫМ сэмплам — дёшево (≈0.14 мс на чанк).
        {
            let mut vguard = a.vad.lock();
            let Some(v) = vguard.as_mut() else { continue };
            while vad_pos + crate::vad::CHUNK <= mono16.len() {
                let p = v.process_chunk(&mono16[vad_pos..vad_pos + crate::vad::CHUNK]).unwrap_or(0.0);
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
            continue; // в активном сегменте речи ещё нет — ASR не дёргаем
        }
        let silence_samples = vad_pos.saturating_sub(last_speech_end);
        let close_segment = silence_samples >= SIL_BOUND_MS * 16
            || mono16.len().saturating_sub(seg_start) >= MAX_SEG_SAMPLES;

        // try_lock: финал уже забрал модель → тик пропускаем.
        let Some(mut g) = a.gigaam.try_lock() else { continue };
        let Some(gm) = g.as_mut() else { continue };
        let (committed, volatile) = if close_segment {
            // Граница: последний речевой чанк + 300 мс хвоста.
            let bound = (last_speech_end + 4800).min(mono16.len());
            let txt = gm.transcribe(&mono16[seg_start..bound]).unwrap_or_default();
            drop(g);
            let t = txt.trim().to_string();
            if !t.is_empty() {
                // Пауза перед сегментом >=2 c -> абзац. Переговорённая заново
                // фраза ЗАМЕНЯЕТ предыдущий сегмент, а не дописывается дважды.
                let gap = cur_seg_first_speech.unwrap_or(prev_seg_end).saturating_sub(prev_seg_end);
                let para = !committed_segs.is_empty() && gap >= PARA_GAP_SAMPLES;
                match committed_segs.last_mut() {
                    Some(last) if is_restatement(&last.1, &t) => last.1 = t,
                    _ => committed_segs.push((para, t)),
                }
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

        if last_emitted.as_ref() == Some(&(committed.clone(), volatile.clone())) {
            continue; // ничего нового — не дёргаем фронт
        }
        last_emitted = Some((committed.clone(), volatile.clone()));
        let full = match (committed.is_empty(), volatile.is_empty()) {
            (false, false) => format!("{committed} {volatile}"),
            (false, true) => committed.clone(),
            (true, false) => volatile.clone(),
            (true, true) => continue,
        };
        if a.stop.load(Ordering::Acquire) {
            break; // не показываем партиал поверх идущего финала
        }
        let _ = a.app.emit(
            "partial",
            serde_json::json!({ "text": full, "committed": committed, "volatile": volatile, "seq": a.seq }),
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
                let already = a.committed_field.lock().clone();
                if flat == already || !live_target_ok(&a.start_fp, &a.abort) {
                    continue;
                }
                let _inj = a.inject_lock.lock();
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
    start_fp: (String, String),
    stream_mode: String,
    /// Поколение (seq) диктовки — кладётся в событие "partial" для отбрасывания
    /// устаревших партиалов на фронте.
    seq: u64,
}

/// Фоновая петля живого стриминга: каждые ~700 мс снимает буфер, ресэмплит,
/// гонит через whisper-server БЕЗ гейта и эмитит "partial"; для auto/always
/// дополнительно вставляет текст в поле клавишами.
///
/// Поток НЕ владеет cpal Stream (тот живёт на потоке движка) — через границу
/// потока переходит лишь Arc на буфер сэмплов, поэтому он полностью Send.
fn partial_loop(a: PartialLoopArgs) {
    let min_new = (a.rate as f32 * 0.3) as usize; // нужно ≥0.3 c нового звука на тик (было 0.4 — свежее)
    let mut last_len = 0usize;
    // Стабилизатор живого префикса: история N=6 партиалов, фиксация по K=2 совпавшим
    // подряд тикам. Монотонный committed → пилюля не переписывает уже показанное
    // начало/середину фразы. Локален для диктовки (новый Start → новая петля → сброс).
    let mut stab = PrefixStabilizer::new(6, 2);

    loop {
        std::thread::sleep(Duration::from_millis(500)); // каденс тиков (было 700 — отзывчивее)
        if a.stop.load(Ordering::Acquire) {
            break;
        }

        // Снимок буфера: читаем длину и КЛОНИРУЕМ Vec (НЕ сливаем — финал ждёт полный буфер).
        let snapshot: Vec<f32> = {
            match a.buffer.lock() {
                Ok(g) => {
                    if g.len() < last_len + min_new {
                        continue; // мало нового звука — пропускаем тик
                    }
                    g.clone()
                }
                Err(_) => continue,
            }
        };
        // last_len двигаем НЕ здесь, а после успешного распознавания (ниже): иначе
        // пропущенный тик (занят asr_lock / звук обрезан в тишину) «съедал» бы порог
        // и партиал откладывался до накопления ещё 0.4 c звука.

        // Ресэмпл + лёгкая обрезка тишины; слишком короткий звук пропускаем.
        let mono16 = audio::resample_to_16k(&snapshot, a.rate);
        let trimmed = audio::trim_silence(&mono16, 16000);
        if trimmed.len() < 16000 * 3 / 10 {
            continue; // < ~0.3 c полезного звука
        }

        let wav = paths::tmp_dir().join("partial.wav");
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
        last_len = snapshot.len();

        // Стабилизируем префикс: committed (чёрный, монотонный) + volatile (серый хвост).
        let (committed, volatile) = stab.push(&txt);
        let full = match (committed.is_empty(), volatile.is_empty()) {
            (false, false) => format!("{committed} {volatile}"),
            (false, true) => committed.clone(),
            (true, false) => volatile.clone(),
            (true, true) => String::new(),
        };

        // Пилюля стримит разделённо: text (=committed+volatile) для обратной
        // совместимости, committed/volatile — новый контракт (стабильный + хвост).
        let _ = a.app.emit(
            "partial",
            serde_json::json!({ "text": full, "committed": committed, "volatile": volatile, "seq": a.seq }),
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
}

/// Проверка отпечатка окна перед живой вставкой: при смене окна/поля — навсегда
/// (на эту диктовку) выставляем abort и больше не вставляем. Возвращает true,
/// если вставлять МОЖНО (окно то же и abort не выставлен).
fn live_target_ok(start_fp: &(String, String), abort: &Arc<AtomicBool>) -> bool {
    if abort.load(Ordering::Acquire) {
        return false;
    }
    let cur = crate::app_context::detect();
    if (cur.exe, cur.title) != *start_fp {
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
    let already = a.committed.lock().clone();
    if committed == already {
        return; // фиксировать нечего нового
    }
    if !live_target_ok(&a.start_fp, &a.abort) {
        return;
    }
    // Замок эмиссии клавиш (как в always).
    let _inj = a.inject_lock.lock();
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
    // finish() дропает cpal Stream и забирает полный буфер.
    let samples = c.finish();
    ctx.recording.store(false, Ordering::SeqCst);
    // Поколение ЭТОЙ диктовки — финал-поток сверит его перед вставкой (C4).
    let my_gen = ctx.gen.load(Ordering::SeqCst);

    // Останавливаем петлю частичных результатов и ДОЖИДАЕМСЯ её —
    // ни один тик не идёт во время финального прохода.
    let pstate = ctx.partial.lock().take();
    if let Some(mut st) = pstate {
        st.stop.store(true, Ordering::Release);
        if let Some(j) = st.join.take() {
            let _ = j.join();
        }
        // Переносим живое состояние в финальный проход (для inject_incremental реконсиляции).
        let live = LiveState {
            stream_mode: st.stream_mode,
            injected: st.injected,
            committed: st.committed,
            abort: st.abort,
            start_fp: st.start_fp,
        };
        if ctx.settings.lock().play_sounds {
            sound::play(false);
        }
        set_status(&ctx.app, "transcribing");
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            if let Err(err) = process_utterance(&ctx2, samples, rate, Some(live), my_gen) {
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
    set_status(&ctx.app, "transcribing");

    // Тяжёлую обработку выносим в отдельный поток, чтобы движок мог принять новую запись.
    let ctx2 = ctx.clone();
    std::thread::spawn(move || {
        if let Err(err) = process_utterance(&ctx2, samples, rate, None, my_gen) {
            log::error!("process_utterance: {err:#}");
            report_process_err(&ctx2.app, &err);
        }
        if ctx2.gen.load(Ordering::SeqCst) == my_gen {
            set_status(&ctx2.app, "idle");
        }
    });
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
    start_fp: (String, String),
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

fn process_utterance(
    ctx: &EngineCtx,
    samples: Vec<f32>,
    rate: u32,
    live: Option<LiveState>,
    my_gen: u64,
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
    let pre_ms = t_pre.elapsed().as_millis() as u64;
    if trimmed.len() < 16000 / 5 {
        // < ~0.2 c полезного звука — считаем, что речи не было
        return Ok(());
    }

    // Уникальное имя WAV на диктовку (C4): исключает гонку на общем файле, когда
    // финал предыдущей диктовки ещё в полёте, а уже стартовала следующая.
    let wav = paths::tmp_dir().join(format!("utterance_{my_gen}.wav"));
    audio::write_wav_16k_mono(&wav, &trimmed)?;

    // Словарь и сниппеты из БД (под локом).
    let (dict, snippets) = {
        let conn = ctx.db.lock();
        (load_dict(&conn), load_snippets(&conn))
    };

    // ── ASR: приоритет облачного STT-провайдера, иначе Gemini, иначе локальный whisper ──
    // Финальный whisper-проход сериализуем тем же asr_lock, что и тики partial,
    // чтобы он никогда не пересёкся с частичным запросом (петля к этому моменту
    // уже остановлена и приджойнена — это пояс поверх подтяжек).
    //
    // ВАЖНО: облачный текст НЕ проходит whisper-гейт уверенности (тот завязан на
    // verbose_json локального whisper). Пустой ответ облака трактуем как norecog
    // ниже — общим путём (как и при отклонении гейта локальным проходом).
    // Avalon — основной STT по умолчанию, НО без ключа НЕ делаем бессмысленных сетевых
    // попыток (лишний RTT/таймаут): мгновенно работаем на локальном whisper. «Умный
    // фолбэк» из решения пользователя: облако активно, только когда ключ реально есть.
    let cloud_key_ok = match s.stt_provider.as_str() {
        "openai_compat" => !s.resolve_oai_key().is_empty(),
        "deepgram" => !s.resolve_deepgram_key().is_empty(),
        _ => false,
    };
    let use_cloud_stt =
        (s.stt_provider == "openai_compat" || s.stt_provider == "deepgram") && cloud_key_ok;
    let use_cloud_gemini =
        s.cloud_asr && s.ai_backend == "gemini" && crate::gemini::available(&s.ai_api_key);
    let t0 = Instant::now();
    let raw = if use_cloud_stt {
        // Облачный провайдер — основной путь. Сетевой вызов БЕЗ asr_lock
        // (asr_lock сериализует только whisper-server; облако к нему не обращается).
        match crate::cloud_stt::transcribe(&s, &wav) {
            Ok(t) if !t.trim().is_empty() => {
                emit_stt_mode(&ctx.app, &s.stt_provider, false);
                t
            }
            res => {
                // Ошибка ИЛИ пустой ответ → решаем по флагу fallback.
                match res {
                    Err(e) => log::warn!("облачный STT ({}) ошибка: {e}", s.stt_provider),
                    Ok(_) => log::warn!("облачный STT ({}) вернул пусто", s.stt_provider),
                }
                if s.stt_fallback_local {
                    log::warn!("облачный STT недоступен — откат на локальный whisper");
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
            Ok(t) => t,
            Err(e) => {
                log::warn!("облачный ASR (Gemini) ошибка: {e}; откат на локальный whisper");
                let _g = ctx.asr_lock.lock();
                local_transcribe(ctx, &s, &dict, &wav)?
            }
        }
    } else {
        local_asr(ctx, &s, &dict, &wav, &trimmed)?
    };
    let ms = t0.elapsed().as_millis() as u64;

    if raw.trim().is_empty() {
        // Гейт уверенности отклонил (невнятно / тишина / чужой язык).
        // Если в режиме always/auto мы УЖЕ напечатали лучший partial — НЕ стираем
        // экран (никакого mass-backspace), оставляем как есть; иначе старое поведение.
        if live_inserted {
            dbg_log("финал отклонён, но живой текст уже вставлен — не стираем");
        }
        if s.play_sounds {
            sound::fail();
        }
        let _ = ctx.app.emit(
            "norecog",
            serde_json::json!({ "message": "Не расслышал — повторите чётче" }),
        );
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // Постобработка (правила) + выученные исправления.
    let t_post = Instant::now();
    let corrections = {
        let conn = ctx.db.lock();
        load_corrections(&conn)
    };
    let mut text = postprocess::process(&raw, &s, &dict, &snippets);
    text = postprocess::apply_corrections(&text, &corrections);
    let post_ms = t_post.elapsed().as_millis() as u64;
    if text.trim().is_empty() {
        // Постобработка съела весь текст — экран не трогаем (как и при отклонении гейта).
        let _ = std::fs::remove_file(&wav);
        return Ok(());
    }

    // ── «Умный» рерайт под стиль активного приложения (Gemini или локальный Ollama) ──
    // Контекст окна нужен и для тона, и для payload Ollama — детектим один раз.
    // Тон = категория приложения (Gmail→formal, мессенджеры→casual, нейросети→ai), либо ручной тон.
    let actx = crate::app_context::detect();
    dbg_log(&format!("app: exe={} title={:?} → {}", actx.exe, actx.title, actx.category));
    // Тон по приложению считаем через category_for — он учитывает пользовательские
    // app_profile_overrides (ветка B) ПЕРЕД встроенной таблицей классификации.
    let tone = if s.tone_by_app {
        crate::app_context::category_for(&actx.exe, &actx.title, &s.app_profile_overrides)
    } else {
        s.tone.clone()
    };
    // verbatim и нейтральный/пустой профиль LLM не зовут (правила уже отработали).
    // "ai" (окна нейросетей: Claude/ChatGPT/…) тоже БЕЗ LLM: GigaAM e2e уже отдаёт
    // чистый пунктуированный текст, пригодный как промпт, а синхронный рерайт
    // добавлял секунды поверх ~100мс распознавания — именно туда пользователь
    // диктует чаще всего (просьба «ускорь распознавание в категории ИИ», 10.06).
    let llm_eligible = !s.verbatim
        && !tone.is_empty()
        && tone != "neutral"
        && tone != "verbatim"
        && tone != "ai"
        && tone != "code";
    let t_llm = Instant::now();
    if llm_eligible {
        if s.ai_backend == "gemini" && crate::gemini::available(&s.ai_api_key) {
            // Облачный рефайн через Gemini.
            let instruction = build_tone_instruction(&tone, &corrections);
            match crate::gemini::refine(&s.ai_api_key, &s.ai_model, &instruction, &text) {
                Ok(refined) if !refined.trim().is_empty() => text = refined.trim().to_string(),
                Ok(_) => {}
                Err(e) => log::warn!("Gemini рерайт ошибка: {e}"),
            }
        } else if s.ai_backend == "ollama" && crate::ollama::configured(&s.ollama_url) {
            // Локальный офлайн-рефайн через Qwen3 (Ollama).
            let user = build_voiceflow_payload(&actx, &text);
            match crate::ollama::refine(
                &s.ollama_url,
                &s.ollama_model,
                crate::ollama::SYSTEM_PROMPT,
                &user,
            ) {
                Ok(r) if !r.trim().is_empty() => text = r.trim().to_string(),
                Ok(_) => {}
                Err(e) => log::warn!("Ollama рерайт ошибка: {e}; оставляем правила"),
            }
        } else if s.ai_backend == "openai_compat" && crate::rewrite::configured(&s) {
            // Облачный рефайн через OpenAI-совместимый chat (Claude Haiku/OpenAI/Groq/…).
            // Тот же системный промпт (voiceflow_ru.txt) и payload [ПРИЛОЖЕНИЕ]/[ДИКТОВКА],
            // что и у Ollama — единый few-shot, прокси-aware, жёсткий таймаут. Без ключа
            // refine() вернёт Err → graceful-деградация на текст после правил.
            let user = build_voiceflow_payload(&actx, &text);
            match crate::rewrite::refine(&s, crate::ollama::SYSTEM_PROMPT, &user) {
                Ok(r) if !r.trim().is_empty() => text = r.trim().to_string(),
                Ok(_) => {}
                Err(e) => log::warn!("OpenAI-compat рерайт ошибка: {e}; оставляем правила"),
            }
        }
        // Бэкенд off или движок недоступен — graceful-деградация: текст после правил.
    }
    let llm_ms = t_llm.elapsed().as_millis() as u64;

    // C5: после apply_corrections и LLM-рерайта пробелы могли «съехать»
    // (replace_ci — сырая подстрочная замена; LLM иногда добавляет лишние пробелы).
    // normalize_spaces внутри postprocess::process отрабатывает РАНЬШЕ этих шагов,
    // поэтому нормализуем ещё раз — финально, перед вставкой.
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
    let live_mode = live.as_ref().map(|l| l.stream_mode.as_str()).unwrap_or("never");
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
    let t_inj = Instant::now();
    match (live.as_ref(), live_mode) {
        (Some(l), "always") | (Some(l), "auto") => {
            let cur = crate::app_context::detect();
            if l.abort.load(Ordering::Acquire) {
                dbg_log("финал: окно сменилось — реконсиляцию пропускаем");
            } else if (cur.exe, cur.title) != l.start_fp {
                l.abort.store(true, Ordering::Release);
                dbg_log("финал: целевое окно изменилось — реконсиляцию пропускаем");
            } else {
                // prev = что уже физически в поле (injected для always, committed для auto).
                let prev = if l.stream_mode == "always" {
                    l.injected.lock().clone()
                } else {
                    l.committed.lock().clone()
                };
                // Клавишная реконсиляция — без абзацев (\n печатался бы Enter-ом
                // и в чатах отправлял сообщение). Абзацы — только в clipboard-пути.
                let flat = flatten_breaks(&text);
                if let Err(e) = inject::inject_incremental(&prev, &flat) {
                    log::warn!("финальная реконсиляция: {e}");
                } else if l.stream_mode == "always" {
                    *l.injected.lock() = flat;
                } else {
                    *l.committed.lock() = flat;
                }
            }
        }
        _ => {
            // never-режим или петли не было — поведение как раньше (вставка целиком).
            // Ошибку пробрасываем ПОСЛЕ уборки временного WAV (иначе утечка в tmp).
            if let Err(e) = inject::inject(&text, &s.paste_method) {
                drop(inject_guard);
                let _ = std::fs::remove_file(&wav);
                return Err(e);
            }
        }
    }
    drop(inject_guard); // освобождаем замок клавиш сразу после вставки
    let inject_ms = t_inj.elapsed().as_millis() as u64;
    // Сквозной замер этапов финала: отпускание клавиши → текст в поле.
    dbg_log(&format!(
        "[lat] gen={my_gen} pre={pre_ms}мс asr={ms}мс post={post_ms}мс llm={llm_ms}мс inject={inject_ms}мс total={}мс",
        t_all.elapsed().as_millis()
    ));
    // Запомнить вставленное — для авто-захвата исправлений из буфера (во всех путях).
    *ctx.last_inject.lock() = Some(text.clone());

    let words = text.split_whitespace().count() as u32;
    {
        let conn = ctx.db.lock();
        let _ = db::record_dictation(&conn, &text, "", words, ms);
    }
    // Персонализация: сохраняем пару (аудио ↔ текст) в датасет.
    if s.personalize {
        save_sample(ctx, &wav, &text);
    }
    // Убираем за собой уникальный временный WAV этой диктовки (C4).
    let _ = std::fs::remove_file(&wav);
    let _ = ctx
        .app
        .emit("transcript", serde_json::json!({ "text": text, "ms": ms, "words": words, "seq": my_gen }));
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
    dbg_log(&format!("gigaam: загружен за {} мс", t.elapsed().as_millis()));
    *guard = Some(g);
    Ok(())
}

/// Гарантировать запущенный whisper-server с нужной моделью; вернуть порт.
fn ensure_server(
    ctx: &EngineCtx,
    whisper_dir: &std::path::Path,
    model: &std::path::Path,
    threads: u32,
) -> anyhow::Result<u16> {
    const PORT: u16 = 8771;
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
        let srv = asr::start_server(whisper_dir, model, PORT, threads)?;
        *guard = Some(srv);
    }
    Ok(PORT)
}

/// Типизированная ошибка «модель не установлена» — чтобы отличать её от прочих
/// сбоев (микрофон/сервер) и показывать специальное предупреждение «Выберите модель»,
/// а не глотать в общий "error". Матчится через `err.downcast_ref::<ModelMissing>()`.
#[derive(Debug)]
struct ModelMissing;
impl std::fmt::Display for ModelMissing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Модель не установлена — скачайте её во вкладке «Модель».")
    }
}
impl std::error::Error for ModelMissing {}

/// true, если движку нечем распознавать: для gigaam — нет файлов модели GigaAM
/// (и нет whisper-фолбэка), для whisper — ни выбранной модели, ни одного *.bin.
fn no_model_installed(s: &Settings) -> bool {
    if s.engine == "gigaam" && crate::gigaam::dir_ready(&paths::gigaam_dir()) {
        return false;
    }
    if paths::model_path(&s.model).exists() {
        return false;
    }
    if let Ok(rd) = std::fs::read_dir(paths::models_dir()) {
        for entry in rd.flatten() {
            if entry.path().extension().and_then(|x| x.to_str()) == Some("bin") {
                return false;
            }
        }
    }
    true
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
        log::warn!("модель {} не найдена, fallback → {:?}", s.model, pp.file_name());
        return Ok(pp);
    }
    Err(anyhow::Error::new(ModelMissing))
}

fn load_dict(conn: &Connection) -> Vec<postprocess::Dict> {
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT term, replacement FROM dictionary") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(postprocess::Dict { term: r.get(0)?, replacement: r.get(1)? })
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

fn emit_error(app: &AppHandle, msg: &str) {
    let _ = app.emit("error", serde_json::json!({ "message": msg }));
}

/// Сообщить фронту, какой STT-движок отработал финал и работали ли офлайн.
/// engine — "openai_compat" | "deepgram" | "local"; offline=true только для
/// локального whisper (нет сети). Пилюля показывает индикатор облако/офлайн.
fn emit_stt_mode(app: &AppHandle, engine: &str, offline: bool) {
    let _ = app.emit("stt_mode", serde_json::json!({ "engine": engine, "offline": offline }));
}

/// Специальное предупреждение «модель не выбрана/не установлена» — фронт показывает
/// баннер с кнопкой перехода на вкладку «Модель», overlay дублирует кратко.
fn emit_no_model(app: &AppHandle) {
    let _ = app.emit(
        "no_model",
        serde_json::json!({ "message": "Выберите модель во вкладке «Модель»" }),
    );
}

/// Локальный ASR с роутингом: GigaAM для русского (VAD-гейт тишины, сегментация
/// длинного аудио, мягкий откат на whisper при любой ошибке), иначе whisper.
fn local_asr(
    ctx: &EngineCtx,
    s: &Settings,
    dict: &[postprocess::Dict],
    wav: &std::path::Path,
    samples_16k: &[f32],
) -> anyhow::Result<String> {
    if s.engine == "gigaam" && s.language == "ru" {
        // Гейт тишины: нет речи → пустой raw → общий norecog-путь (как у гейта
        // уверенности whisper). RNNT не галлюцинирует как whisper, поэтому
        // отдельного пословного гейта не нужно.
        let t_vad = Instant::now();
        let speech = match ctx.vad_final.lock().as_mut() {
            Some(v) => v.has_speech(samples_16k, 0.5).unwrap_or(true),
            None => true,
        };
        let vad_ms = t_vad.elapsed().as_millis() as u64;
        if !speech {
            dbg_log(&format!("[lat] vad={vad_ms}мс: речи нет — отклонено без ASR"));
            return Ok(String::new());
        }
        match ensure_gigaam(ctx, s) {
            Ok(()) => {
                let mut guard = ctx.gigaam.lock();
                if let Some(g) = guard.as_mut() {
                    match gigaam_transcribe_long(g, &ctx.vad_final, samples_16k) {
                        Ok(t) => {
                            let st = g.last_stats;
                            emit_stt_mode(&ctx.app, "gigaam", false);
                            dbg_log(&format!(
                                "[lat] vad={vad_ms}мс gigaam: audio={}мс frontend={}мс encoder={}мс decoder={}мс asr={}мс",
                                st.audio_ms, st.frontend_ms, st.encoder_ms, st.decoder_ms, st.total_ms
                            ));
                            return Ok(t);
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
        }
    }
    let _g = ctx.asr_lock.lock();
    local_transcribe(ctx, s, dict, wav)
}

/// GigaAM-финал с разметкой: VAD делит запись на фразы (тишина >=600 мс),
/// фразы длиннее 25 c дорезаются по ближайшей тишине (лимит pos_emb модели).
/// Пауза >=2 c между фразами -> абзац ("\n\n"); переговорённая заново фраза
/// заменяет предыдущую (is_restatement) — та же логика, что в живой петле.
/// pub(crate): используется и финалом, и headless `--gigaam-selftest` (lib.rs).
pub(crate) fn gigaam_transcribe_long(
    g: &mut crate::gigaam::GigaAm,
    vad: &Arc<Mutex<Option<crate::vad::SileroVad>>>,
    samples: &[f32],
) -> anyhow::Result<String> {
    const SIL_SPLIT: usize = 600 * 16; // межфразная тишина
    const PARA_GAP: usize = 2 * 16000; // абзац
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
            return g.transcribe(samples);
        }
        let mut parts = Vec::new();
        let mut start = 0usize;
        while start < samples.len() {
            let cut = (start + MAX_SEG).min(samples.len());
            let t = g.transcribe(&samples[start..cut])?;
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
            let t = g.transcribe(&samples[start..cut])?;
            if !t.trim().is_empty() {
                texts.push(t.trim().to_string());
            }
            start = cut;
        }
        let t = texts.join(" ");
        if t.is_empty() {
            continue;
        }
        let para = !segs.is_empty() && u.gap_before >= PARA_GAP;
        match segs.last_mut() {
            Some(last) if is_restatement(&last.1, &t) => last.1 = t,
            _ => segs.push((para, t)),
        }
    }
    Ok(render_segments(&segs))
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
    fn flatten_breaks_for_keyboard() {
        assert_eq!(flatten_breaks("а\n\nб  в"), "а б в");
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
            Ok(postprocess::Correction { wrong: r.get(0)?, right: r.get(1)? })
        }) {
            out.extend(rows.flatten());
        }
    }
    out
}

/// Пользовательский payload для системного промпта Ollama (Qwen3).
/// [ОКРУЖЕНИЕ] не отправляем — оно пустое. Имя приложения берём из заголовка
/// окна, при пустом — из exe, иначе "неизвестно".
fn build_voiceflow_payload(actx: &crate::app_context::AppContext, text: &str) -> String {
    let app = if !actx.title.trim().is_empty() {
        actx.title.as_str()
    } else if !actx.exe.is_empty() {
        actx.exe.as_str()
    } else {
        "неизвестно"
    };
    format!("[ПРИЛОЖЕНИЕ]: {app}\n[ДИКТОВКА]: {text}")
}

/// Инструкция для Gemini: переписать текст в нужном стиле, без отсебятины.
fn build_tone_instruction(tone: &str, corrections: &[postprocess::Correction]) -> String {
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
         Сохрани смысл и язык (русский). Исправь ошибки распознавания, опечатки и пунктуацию. \
         НЕ добавляй ничего от себя, не отвечай на текст и не комментируй — верни ТОЛЬКО переписанный текст."
    );
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

/// Монитор буфера обмена: если пользователь скопировал отредактированную версию
/// последней вставки — выучить пословные исправления (распознано → правильно).
fn clipboard_monitor(ctx: EngineCtx) {
    let mut last_seen = arboard::Clipboard::new()
        .ok()
        .and_then(|mut c| c.get_text().ok())
        .unwrap_or_default();
    loop {
        std::thread::sleep(Duration::from_millis(1300));
        // Пока инжектор печатает/работает с буфером — не лезем в clipboard
        // (contention с arboard внутри вставки = подвисания и порча восстановления).
        if inject::is_busy() {
            continue;
        }
        let cur = match arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()) {
            Some(t) => t,
            None => continue,
        };
        if cur == last_seen || cur.trim().is_empty() {
            continue;
        }
        last_seen = cur.clone();
        let injected = ctx.last_inject.lock().clone();
        if let Some(inj) = injected {
            if cur.trim() == inj.trim() {
                continue; // это наш же текст — не учим
            }
            try_learn(&ctx, &inj, &cur);
        }
    }
}

/// Выучить исправления из пары (вставлено → отредактировано пользователем).
fn try_learn(ctx: &EngineCtx, injected: &str, edited: &str) {
    let wt: Vec<&str> = injected.split_whitespace().collect();
    let wv: Vec<&str> = edited.split_whitespace().collect();
    if wt.is_empty() || wv.is_empty() {
        return;
    }
    // Похожесть (Jaccard по словам, нижний регистр): правка, а не другой текст.
    let st: HashSet<String> = wt.iter().map(|w| w.to_lowercase()).collect();
    let sv: HashSet<String> = wv.iter().map(|w| w.to_lowercase()).collect();
    let common = st.intersection(&sv).count();
    let denom = st.len().max(sv.len()).max(1);
    let sim = common as f64 / denom as f64;
    if !(0.5..1.0).contains(&sim) {
        return; // не похоже на правку (или идентично)
    }
    // Пословные замены при равной длине.
    if wt.len() == wv.len() {
        let conn = ctx.db.lock();
        let mut learned = 0u32;
        for (a, b) in wt.iter().zip(wv.iter()) {
            let ca = a.trim_matches(|c: char| !c.is_alphanumeric());
            let cb = b.trim_matches(|c: char| !c.is_alphanumeric());
            if ca.is_empty() || cb.is_empty() || ca.to_lowercase() == cb.to_lowercase() {
                continue;
            }
            if db::add_correction(&conn, ca, cb).is_ok() {
                learned += 1;
            }
        }
        if learned > 0 {
            dbg_log(&format!("выучено исправлений: {learned}"));
            let _ = ctx.app.emit("learned", serde_json::json!({ "count": learned }));
        }
    }
}
