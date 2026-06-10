//! VoxFlow — локальный голосовой ввод. Точка сборки Tauri-приложения.

mod app_context;
mod asr;
mod audio;
mod cloud_stt;
mod commands;
mod db;
mod engine;
mod gemini;
mod gigaam;
mod hotkey;
mod inject;
mod models;
mod net;
mod ollama;
mod parakeet;
mod paths;
mod postprocess;
mod rewrite;
mod settings;
mod vad;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, PhysicalPosition, PhysicalSize};
use tauri_plugin_autostart::MacosLauncher;

use commands::AppState;
use engine::EngineCmd;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        // ПЕРВЫМ: единственный экземпляр процесса. Иначе старый и новый voxflow.exe
        // открывают ОДИН voxflow.db и затирают настройки друг друга (B4). Колбэк
        // показывает/фокусирует уже запущенное окно настроек.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(main) = app.get_webview_window("main") {
                let _ = main.show();
                let _ = main.unminimize();
                let _ = main.set_focus();
            }
        }))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--autostart"]),
        ))
        .setup(|app| {
            let handle = app.handle().clone();

            // БД + настройки
            let conn = db::open().expect("open db");
            let loaded = settings::load(&conn);
            let db_arc = Arc::new(Mutex::new(conn));
            let settings_arc = Arc::new(Mutex::new(loaded));
            let recording = Arc::new(AtomicBool::new(false));

            // Канал движка
            let (tx, rx) = std::sync::mpsc::channel::<EngineCmd>();

            engine::spawn(
                handle.clone(),
                rx,
                db_arc.clone(),
                settings_arc.clone(),
                recording.clone(),
            );
            hotkey::spawn(tx.clone(), settings_arc.clone());

            let want_autostart = settings_arc.lock().autostart;

            let overlay_hit = Arc::new(Mutex::new(None));
            app.manage(AppState {
                db: db_arc,
                settings: settings_arc,
                engine_tx: Mutex::new(tx),
                recording,
                overlay_hit: overlay_hit.clone(),
            });

            build_tray(&handle)?;
            setup_overlay(&handle);
            spawn_overlay_hover_poller(&handle, overlay_hit);

            // Первый запуск: если русская модель (GigaAM) не установлена —
            // скачать автоматически с прогрессом в UI (задача №5 брифа).
            models::ensure_default_models(handle.clone());

            // Применить автозапуск согласно сохранённым настройкам (reconcile с ОС).
            {
                use tauri_plugin_autostart::ManagerExt;
                let mgr = handle.autolaunch();
                let res = if want_autostart { mgr.enable() } else { mgr.disable() };
                if let Err(e) = res {
                    log::error!("autostart reconcile ({want_autostart}) failed: {e}");
                }
            }

            // Показать окно настроек при запуске, НО не при автозапуске (тогда — в трей).
            let autostarted = std::env::args().any(|a| a == "--autostart");
            if !autostarted {
                if let Some(main) = handle.get_webview_window("main") {
                    let _ = main.show();
                    let _ = main.set_focus();
                }
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Закрытие окна настроек прячет его в трей, а не завершает приложение.
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            // Overlay перетаскивают мышью — запоминаем позицию (debounce 600 мс:
            // пишем только последнюю точку серии, не каждый пиксель драга).
            if window.label() == "overlay" {
                if let tauri::WindowEvent::Moved(pos) = event {
                    use std::sync::atomic::{AtomicU64, Ordering as AO};
                    static MOVE_SEQ: AtomicU64 = AtomicU64::new(0);
                    let seq = MOVE_SEQ.fetch_add(1, AO::SeqCst) + 1;
                    let app = window.app_handle().clone();
                    let (x, y) = (pos.x, pos.y);
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(600));
                        if MOVE_SEQ.load(AO::SeqCst) == seq {
                            if let Some(state) = app.try_state::<AppState>() {
                                let conn = state.db.lock();
                                let _ = db::kv_set(&conn, "overlay_pos", &format!("[{x},{y}]"));
                            }
                        }
                    });
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::list_audio_devices,
            commands::list_models,
            commands::download_model,
            commands::delete_model,
            commands::toggle_dictation,
            commands::is_recording,
            commands::get_stats,
            commands::get_history,
            commands::dictionary_list,
            commands::dictionary_upsert,
            commands::dictionary_delete,
            commands::snippet_list,
            commands::snippet_upsert,
            commands::snippet_delete,
            commands::show_main_window,
            commands::ai_test,
            commands::stt_test,
            commands::corrections_list,
            commands::corrections_upsert,
            commands::corrections_delete,
            commands::overlay_box,
            commands::overlay_hit,
        ])
        .run(tauri::generate_context!())
        .expect("error while running VoxFlow");
}

/// Диагностический прогон ASR + постобработки на готовом 16 кГц WAV (без GUI/микрофона).
/// Используется как `voxflow.exe --selftest <wav>` для проверки пайплайна кодом приложения.
pub fn selftest(wav_path: &str) {
    use std::path::Path;
    let conn = db::open().expect("open db");
    let s = settings::load(&conn);
    let whisper_dir = paths::whisper_dir_standalone();
    let model = {
        let m = paths::model_path(&s.model);
        if m.exists() {
            m
        } else {
            std::fs::read_dir(paths::models_dir())
                .ok()
                .and_then(|rd| {
                    rd.flatten()
                        .map(|e| e.path())
                        .find(|p| p.extension().and_then(|x| x.to_str()) == Some("bin"))
                })
                .expect("нет ни одной модели в models_dir")
        }
    };
    eprintln!("[selftest] whisper_dir = {whisper_dir:?}");
    eprintln!("[selftest] model       = {model:?}");
    eprintln!("[selftest] wav         = {wav_path}");
    eprintln!("[selftest] language    = {}, threads = {}", s.language, s.effective_threads());

    let params = asr::AsrParams {
        whisper_dir: &whisper_dir,
        model_path: &model,
        wav_path: Path::new(wav_path),
        language: &s.language,
        threads: s.effective_threads(),
        initial_prompt: None,
    };
    let raw = asr::transcribe_cli(&params).expect("ASR failed");
    println!("RAW   : {raw}");
    let clean = postprocess::process(&raw, &s, &[], &[]);
    println!("CLEAN : {clean}");
}

/// Headless-проверка ЖИВОГО стриминга (без GUI/микрофона): на готовом 16 кГц WAV
/// имитирует петлю частичных результатов, прогоняя НАРАСТАЮЩИЕ срезы аудио
/// (1 c, 2 c, 3 c … до полного) через `asr::transcribe_server_partial` (UNGATED,
/// как в реальной петле), печатая каждый partial + затраченные мс. Затем —
/// финальный ГЕЙТОВАННЫЙ проход (`transcribe_server` + postprocess) с замером.
///
/// Запуск: `voxflow.exe --stream-selftest <16k_mono.wav>`. НЕ открывает окно Tauri.
pub fn stream_selftest(wav_path: &str) {
    use std::time::Instant;

    let conn = db::open().expect("open db");
    let s = settings::load(&conn);
    let whisper_dir = paths::whisper_dir_standalone();
    let model = {
        let m = paths::model_path(&s.model);
        if m.exists() {
            m
        } else {
            std::fs::read_dir(paths::models_dir())
                .ok()
                .and_then(|rd| {
                    rd.flatten()
                        .map(|e| e.path())
                        .find(|p| p.extension().and_then(|x| x.to_str()) == Some("bin"))
                })
                .expect("нет ни одной модели в models_dir")
        }
    };
    let threads = s.effective_threads();
    eprintln!("[stream] whisper_dir = {whisper_dir:?}");
    eprintln!("[stream] model       = {model:?}");
    eprintln!("[stream] wav         = {wav_path}");
    eprintln!(
        "[stream] engine={} nvidia={} language={} threads={}",
        s.engine,
        paths::has_nvidia(),
        s.language,
        threads
    );

    // Считываем WAV целиком в моно f32 16 кГц (hound). Ресэмпл на всякий случай,
    // если кто-то подсунул не 16 кГц.
    let reader = hound::WavReader::open(wav_path).expect("открыть WAV");
    let spec = reader.spec();
    let in_rate = spec.sample_rate;
    let channels = spec.channels as usize;
    let raw_samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .into_samples::<f32>()
            .map(|x| x.unwrap_or(0.0))
            .collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|x| x.unwrap_or(0) as f32 / max)
                .collect()
        }
    };
    // Сводим в моно (среднее по каналам).
    let mono: Vec<f32> = if channels <= 1 {
        raw_samples
    } else {
        raw_samples
            .chunks(channels)
            .map(|fr| fr.iter().sum::<f32>() / fr.len() as f32)
            .collect()
    };
    let full16 = audio::resample_to_16k(&mono, in_rate);
    let dur_s = full16.len() as f32 / 16000.0;
    eprintln!("[stream] длительность аудио ≈ {dur_s:.2} c, сэмплов(16к)={}", full16.len());

    // Поднимаем whisper-server напрямую (вне Tauri-контекста) и ждём готовности.
    const PORT: u16 = 8771;
    eprintln!("[stream] поднимаю whisper-server на :{PORT} …");
    let t_boot = Instant::now();
    let mut srv = match asr::start_server(&whisper_dir, &model, PORT, threads) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[stream] ОШИБКА: сервер не поднялся: {e:#}");
            eprintln!("[stream] (в этом окружении GPU-сборки нет; CPU-сервер может стартовать долго либо упасть)");
            return;
        }
    };
    eprintln!("[stream] сервер готов за {} мс", t_boot.elapsed().as_millis());

    // Прогрев (как в warmup): первый запрос грузит/инициализирует модель.
    {
        let warm = paths::tmp_dir().join("ss_warmup.wav");
        let _ = audio::write_wav_16k_mono(&warm, &vec![0.0f32; 8000]);
        let tw = Instant::now();
        let _ = asr::transcribe_server(PORT, &warm, &s.language, None);
        eprintln!("[stream] прогрев модели: {} мс", tw.elapsed().as_millis());
    }

    // Имитация петли: нарастающие срезы 1 c, 2 c, 3 c … до полного.
    println!("──────── PARTIALS (UNGATED, нарастающие срезы) ────────");
    let step = 16000usize; // 1 секунда
    let mut cut = step;
    let mut tick = 0u32;
    // Тот же стабилизатор, что и в реальной петле — проверяем, что COMMIT растёт
    // монотонно и НЕ переписывает уже показанное (VOL — изменчивый хвост).
    let mut stab = crate::engine::PrefixStabilizer::new(6, 2);
    loop {
        let end = cut.min(full16.len());
        let slice = &full16[..end];
        let trimmed = audio::trim_silence(slice, 16000);
        let wav = paths::tmp_dir().join("ss_partial.wav");
        if audio::write_wav_16k_mono(&wav, &trimmed).is_err() {
            break;
        }
        tick += 1;
        let t = Instant::now();
        let txt = asr::transcribe_server_partial(PORT, &wav, &s.language)
            .unwrap_or_else(|e| format!("<err: {e}>"));
        let ms = t.elapsed().as_millis();
        let sec = end as f32 / 16000.0;
        let (committed, volatile) = stab.push(&txt);
        println!("[p{tick:02} @ {sec:>4.1}c | {ms:>6} мс] COMMIT={committed:?} | VOL={volatile:?}");
        if end >= full16.len() {
            break;
        }
        cut += step;
    }

    // Финальный ГЕЙТОВАННЫЙ проход — как в process_utterance.
    println!("──────── FINAL (gated + postprocess) ────────");
    let final16 = audio::trim_silence(&full16, 16000);
    let wav = paths::tmp_dir().join("ss_final.wav");
    audio::write_wav_16k_mono(&wav, &final16).expect("записать финальный WAV");
    let tf = Instant::now();
    let raw = asr::transcribe_server(PORT, &wav, &s.language, None).unwrap_or_default();
    let ms = tf.elapsed().as_millis();
    println!("[final | {ms} мс] RAW(gated): {raw:?}");
    let clean = postprocess::process(&raw, &s, &[], &[]);
    println!("[final] CLEAN: {clean:?}");
    if raw.trim().is_empty() {
        println!("[final] гейт ОТКЛОНИЛ (пусто) — для тишины/мусора это норма");
    }

    let _ = srv.child.kill();
    eprintln!("[stream] сервер остановлен. Готово.");
}

/// Headless-проверка ОБЛАЧНОГО STT (без GUI/микрофона): на готовом WAV прогоняет
/// `cloud_stt::transcribe` согласно текущему `stt_provider` в настройках, печатает
/// провайдера, RAW-ответ и затраченное время.
///
/// Запуск: `voxflow.exe --stt-test <wav>`. НЕ открывает окно Tauri. Ключ нигде не
/// печатается (`cloud_stt` берёт его сам из настроек/окружения).
pub fn stt_test_cli(wav: &str) {
    use std::path::Path;
    use std::time::Instant;

    let conn = db::open().expect("open db");
    let s = settings::load(&conn);
    eprintln!("[stt-test] provider = {}", s.stt_provider);
    eprintln!("[stt-test] language = {}", s.language);
    eprintln!("[stt-test] proxy    = {}", net::proxy_configured(&s.proxy_url));
    eprintln!("[stt-test] wav      = {wav}");

    let t0 = Instant::now();
    match cloud_stt::transcribe(&s, Path::new(wav)) {
        Ok(text) => {
            let ms = t0.elapsed().as_millis();
            println!("PROVIDER : {}", s.stt_provider);
            println!("RAW      : {text:?}");
            println!("TIME     : {ms} мс");
        }
        Err(e) => {
            let ms = t0.elapsed().as_millis();
            // Ошибку печатаем как есть — cloud_stt не кладёт ключ в текст ошибки.
            println!("PROVIDER : {}", s.stt_provider);
            eprintln!("[stt-test] ОШИБКА за {ms} мс: {e}");
        }
    }
}

/// Поллер «мышь над пилюлей»: каждые 120 мс сравнивает глобальный курсор с
/// зоной пилюли (overlay_hit от фронта, CSS px × scale + позиция окна) и
/// переключает click-through. Вне пилюли окно прозрачно для мыши — кнопки
/// фуллскрин-приложений под оверлеем остаются кликабельными. Во время зажатой
/// ЛКМ состояние не переключаем (не рвать drag пилюли).
fn spawn_overlay_hover_poller(
    app: &tauri::AppHandle,
    hit: Arc<Mutex<Option<(f64, f64, f64, f64)>>>,
) {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetCursorPos(p: *mut [i32; 2]) -> i32;
            fn GetAsyncKeyState(vk: i32) -> i16;
        }
        let app = app.clone();
        std::thread::Builder::new()
            .name("voxflow-hover".into())
            .spawn(move || {
                let mut interactive = false;
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(120));
                    let Some(ov) = app.get_webview_window("overlay") else { continue };
                    let mut pt = [0i32; 2];
                    if unsafe { GetCursorPos(&mut pt) } == 0 {
                        continue;
                    }
                    // ЛКМ зажата (вероятен drag пилюли) — состояние не трогаем.
                    if interactive && unsafe { GetAsyncKeyState(0x01) } as u16 & 0x8000 != 0 {
                        continue;
                    }
                    let (Ok(pos), scale) = (ov.outer_position(), ov.scale_factor().unwrap_or(1.0))
                    else {
                        continue;
                    };
                    // Зона = пилюля (от фронта) либо всё окно, пока репорта нет.
                    let zone = *hit.lock();
                    let (zx, zy, zw, zh) = match zone {
                        Some((x, y, w, h)) => (
                            pos.x as f64 + x * scale,
                            pos.y as f64 + y * scale,
                            w * scale,
                            h * scale,
                        ),
                        None => match ov.outer_size() {
                            Ok(s) => (pos.x as f64, pos.y as f64, s.width as f64, s.height as f64),
                            Err(_) => continue,
                        },
                    };
                    // Гистерезис 8px против дребезга на границе.
                    let pad = if interactive { 8.0 * scale } else { 0.0 };
                    let inside = (pt[0] as f64) >= zx - pad
                        && (pt[0] as f64) <= zx + zw + pad
                        && (pt[1] as f64) >= zy - pad
                        && (pt[1] as f64) <= zy + zh + pad;
                    if inside != interactive {
                        interactive = inside;
                        let _ = ov.set_ignore_cursor_events(!interactive);
                    }
                }
            })
            .ok();
    }
    #[cfg(not(windows))]
    {
        let _ = (app, hit);
    }
}

/// Headless-проверка GigaAM-пайплайна (без GUI/микрофона): грузит VAD+GigaAM,
/// печатает время загрузки/прогрева, имитирует партиал-тики нарастающими срезами
/// и финал с раскладкой по этапам. Запуск: `voxflow.exe --gigaam-selftest <wav>`.
pub fn gigaam_selftest(wav_path: &str) {
    use std::time::Instant;

    eprintln!("[gigaam] модели: {:?}", paths::gigaam_dir());
    let t = Instant::now();
    let mut vad = match vad::SileroVad::load(&paths::vad_model_path(None)) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[gigaam] VAD ОШИБКА: {e:#}");
            return;
        }
    };
    eprintln!("[gigaam] vad загружен за {} мс", t.elapsed().as_millis());
    let t = Instant::now();
    let threads = settings::Settings::default().effective_threads() as usize;
    let mut g = match gigaam::GigaAm::load(&paths::gigaam_dir(), threads) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("[gigaam] ОШИБКА загрузки: {e:#}");
            return;
        }
    };
    eprintln!("[gigaam] загружен за {} мс ({} потоков)", t.elapsed().as_millis(), threads);
    let t = Instant::now();
    let _ = g.transcribe(&vec![0.0f32; 8000]);
    eprintln!("[gigaam] прогрев {} мс", t.elapsed().as_millis());

    // WAV → mono f32 16к.
    let reader = hound::WavReader::open(wav_path).expect("открыть WAV");
    let spec = reader.spec();
    let chans = spec.channels as usize;
    let raw: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.into_samples::<f32>().map(|x| x.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let max = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader.into_samples::<i32>().map(|x| x.unwrap_or(0) as f32 / max).collect()
        }
    };
    let mono: Vec<f32> = if chans <= 1 {
        raw
    } else {
        raw.chunks(chans).map(|f| f.iter().sum::<f32>() / f.len() as f32).collect()
    };
    let full16 = audio::resample_to_16k(&mono, spec.sample_rate);
    eprintln!("[gigaam] аудио {:.2} c", full16.len() as f32 / 16000.0);

    // VAD-гейт.
    let t = Instant::now();
    let speech = vad.has_speech(&full16, 0.5).unwrap_or(true);
    eprintln!("[gigaam] vad-гейт {} мс, речь={}", t.elapsed().as_millis(), speech);

    // Имитация партиал-тиков: нарастающие срезы по 1 c. Для длинных файлов
    // ограничиваемся первыми 20 c (в бою активный сегмент не растёт дольше —
    // петля режет его по VAD-паузам, см. gigaam_partial_loop).
    println!("──────── PARTIALS (нарастающие срезы, ≤20 c) ────────");
    let mut cut = 16000usize;
    let mut tick = 0;
    while cut < full16.len().min(20 * 16000) {
        tick += 1;
        let t = Instant::now();
        let txt = g.transcribe(&full16[..cut]).unwrap_or_default();
        println!("[p{tick:02} @ {:>4.1}c | {:>5} мс] {txt:?}", cut as f32 / 16000.0, t.elapsed().as_millis());
        cut += 16000;
    }

    // Финал — тем же путём, что и боевой process_utterance: длинное аудио
    // режется по VAD-паузам на сегменты ≤25 c (engine::gigaam_transcribe_long).
    println!("──────── FINAL ────────");
    let trimmed = audio::trim_silence(&full16, 16000);
    let vad_arc = std::sync::Arc::new(parking_lot::Mutex::new(Some(vad)));
    let t = Instant::now();
    let txt = engine::gigaam_transcribe_long(&mut g, &vad_arc, &trimmed).unwrap_or_default();
    let wall = t.elapsed().as_millis();
    let st = g.last_stats;
    println!("TEXT  : {txt:?}");
    println!(
        "[lat] audio={}мс (последний сегмент: frontend={}мс encoder={}мс decoder={}мс) финал-стенка {} мс",
        trimmed.len() * 1000 / 16000, st.frontend_ms, st.encoder_ms, st.decoder_ms, wall
    );
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let settings_i = MenuItem::with_id(app, "settings", "Настройки", true, None::<&str>)?;
    let toggle_i = MenuItem::with_id(app, "toggle", "Старт / стоп диктовки", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&settings_i, &toggle_i, &sep, &quit_i])?;

    let _tray = TrayIconBuilder::with_id("voxflow-tray")
        .icon(app.default_window_icon().expect("default icon").clone())
        .menu(&menu)
        .tooltip("VoxFlow — голосовой ввод")
        .on_menu_event(|app, event| match event.id.as_ref() {
            "settings" => {
                let _ = commands::show_main_window(app.clone());
            }
            "toggle" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let _ = state.engine_tx.lock().send(EngineCmd::Toggle);
                }
            }
            "quit" => {
                if let Some(state) = app.try_state::<AppState>() {
                    let _ = state.engine_tx.lock().send(EngineCmd::Shutdown);
                }
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Overlay-индикатор (orb): маленькое интерактивное окно (drag по пилюле,
/// hover-состояния), НЕ click-through — мёртвой зоны нет, потому что окно
/// ужимается под пилюлю (фронт зовёт overlay_box на каждой смене состояния).
/// Позиция запоминается в kv "overlay_pos" и восстанавливается на старте.
fn setup_overlay(app: &tauri::AppHandle) {
    if let Some(ov) = app.get_webview_window("overlay") {
        // По умолчанию click-through: иначе невидимые поля окна перехватывали
        // клики по нижнему центру экрана (кнопки фуллскрин-приложений).
        // Интерактивность включает поллер курсора, только когда мышь над пилюлей.
        let _ = ov.set_ignore_cursor_events(true);
        let scale = ov.scale_factor().unwrap_or(1.0);
        // Стартовый размер = idle-запрос фронта (220×80: запас под hover-рост
        // и тултип); дальше фронт сам зовёт overlay_box на каждом состоянии.
        let win_w = 220.0 * scale;
        let win_h = 80.0 * scale;
        let _ = ov.set_size(PhysicalSize::new(win_w as u32, win_h as u32));
        // Сохранённая позиция, если она в пределах экрана; иначе низ-центр.
        let saved: Option<(i32, i32)> = {
            let state = app.state::<AppState>();
            let conn = state.db.lock();
            db::kv_get(&conn, "overlay_pos").and_then(|j| serde_json::from_str(&j).ok())
        };
        let mut placed = false;
        if let (Some((x, y)), Ok(Some(mon))) = (saved, ov.primary_monitor()) {
            let size = mon.size();
            if x > -64 && y > -64 && x < size.width as i32 && y < size.height as i32 {
                let _ = ov.set_position(PhysicalPosition::new(x, y));
                placed = true;
            }
        }
        if !placed {
            if let Ok(Some(mon)) = ov.primary_monitor() {
                let size = mon.size();
                let x = ((size.width as f64 - win_w) / 2.0) as i32;
                // 64px над низом: выше панели задач, пилюля не прячется за ней.
                let y = (size.height as f64 - win_h - 64.0 * scale) as i32;
                let _ = ov.set_position(PhysicalPosition::new(x, y));
            }
        }
        let _ = ov.show();
    }
}
