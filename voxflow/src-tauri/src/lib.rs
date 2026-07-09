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
mod macos_permissions;
mod models;
mod net;
mod ollama;
mod parakeet;
mod paths;
mod postprocess;
mod rewrite;
mod settings;
mod system_audio;
mod updater;
mod vad;
mod voice_cmds;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, PhysicalPosition, PhysicalSize};
use tauri_plugin_autostart::MacosLauncher;

use commands::{AppState, OverlayHitRect};
use engine::EngineCmd;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // CLI-селфтесты Parakeet/LID. Диспатч здесь, а не в main.rs: main.rs правится
    // другим кластером, а неизвестные ему флаги всё равно проваливаются в run().
    {
        let args: Vec<String> = std::env::args().collect();
        if args.len() >= 3 && args[1] == "--parakeet-selftest" {
            parakeet_selftest(&args[2]);
            return;
        }
        if args.len() >= 3 && args[1] == "--lid-selftest" {
            lid_selftest(&args[2]);
            return;
        }
    }
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
            let autostarted = std::env::args().any(|a| a == "--autostart");

            // БД + настройки. P2-7: не молчаливый краш через .expect, а понятное
            // окно (permission denied / занятый файл / диск) и корректный выход.
            let conn = match db::open() {
                Ok(c) => c,
                Err(e) => {
                    fatal_startup_error(&format!(
                        "Не удалось открыть базу данных VoxFlow:\n{e:#}\n\n\
                         Проверьте права доступа к папке\n{}\nи свободное место на диске.",
                        paths::data_dir().display()
                    ));
                    std::process::exit(1);
                }
            };
            let loaded = settings::load(&conn);
            let db_arc = Arc::new(Mutex::new(conn));
            let settings_arc = Arc::new(Mutex::new(loaded));
            let recording = Arc::new(AtomicBool::new(false));

            // Канал движка
            let (tx, rx) = std::sync::mpsc::channel::<EngineCmd>();

            let engine = engine::spawn(
                handle.clone(),
                rx,
                db_arc.clone(),
                settings_arc.clone(),
                recording.clone(),
            );
            if !autostarted {
                // Поднимаем onboarding до запуска CGEventTap-слушателя, чтобы он
                // не успевал первым открыть Input Monitoring вместо Accessibility.
                macos_permissions::onboard_on_launch(handle.clone());
            }
            hotkey::spawn(tx.clone(), settings_arc.clone(), handle.clone());

            let want_autostart = settings_arc.lock().autostart;

            let overlay_hit = Arc::new(Mutex::new(None));
            app.manage(AppState {
                db: db_arc,
                settings: settings_arc.clone(),
                engine,
                engine_tx: Mutex::new(tx),
                recording,
                overlay_hit: overlay_hit.clone(),
                lang_menu: Mutex::new(None), // заполнит build_tray ниже
            });

            build_tray(&handle)?;
            setup_overlay(&handle);
            spawn_overlay_hover_poller(&handle, overlay_hit);
            spawn_overlay_drag_poller(&handle);

            // Первый запуск/legacy-default: подготовить модель под текущий
            // локальный маршрут. Свежий default — multilingual Whisper auto;
            // явный русский GigaAM продолжает автоподготовку GigaAM.
            let startup_settings = settings_arc.lock().clone();
            models::ensure_default_models(handle.clone(), &startup_settings);

            // Применить автозапуск согласно сохранённым настройкам (reconcile с ОС).
            {
                use tauri_plugin_autostart::ManagerExt;
                let mgr = handle.autolaunch();
                let res = if want_autostart {
                    mgr.enable()
                } else {
                    mgr.disable()
                };
                if let Err(e) = res {
                    log::error!("autostart reconcile ({want_autostart}) failed: {e}");
                }
            }

            // Показать окно настроек при запуске, НО не при автозапуске (тогда — в трей).
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
            // Overlay перетаскивают мышью — после короткой паузы притягиваем к
            // ближайшему якорю и запоминаем уже закреплённую позицию.
            if window.label() == "overlay" {
                if let tauri::WindowEvent::Moved(pos) = event {
                    use std::sync::atomic::{AtomicU64, Ordering as AO};
                    static MOVE_SEQ: AtomicU64 = AtomicU64::new(0);
                    let seq = MOVE_SEQ.fetch_add(1, AO::SeqCst) + 1;
                    let app = window.app_handle().clone();
                    let fallback_pos = (pos.x, pos.y);
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(360));
                        if MOVE_SEQ.load(AO::SeqCst) == seq {
                            let mut save_pos = fallback_pos;
                            if let Some(ov) = app.get_webview_window("overlay") {
                                let cur_pos = ov
                                    .outer_position()
                                    .map(|p| (p.x, p.y))
                                    .unwrap_or(fallback_pos);
                                let win_size = ov
                                    .outer_size()
                                    .map(|s| (s.width as i32, s.height as i32))
                                    .unwrap_or((220, 80));
                                let scale = ov.scale_factor().unwrap_or(1.0);
                                if let Ok(monitors) = ov.available_monitors() {
                                    let work_areas: Vec<_> = monitors
                                        .iter()
                                        .map(|mon| {
                                            let area = mon.work_area();
                                            (
                                                area.position.x,
                                                area.position.y,
                                                area.size.width as i32,
                                                area.size.height as i32,
                                            )
                                        })
                                        .collect();
                                    if let Some(snapped) =
                                        overlay_snap_position(cur_pos, win_size, &work_areas, scale)
                                    {
                                        if snapped != cur_pos {
                                            let _ = ov.set_position(PhysicalPosition::new(
                                                snapped.0, snapped.1,
                                            ));
                                        }
                                        save_pos = snapped;
                                    } else {
                                        save_pos = cur_pos;
                                    }
                                } else {
                                    save_pos = cur_pos;
                                }
                            }
                            if let Some(state) = app.try_state::<AppState>() {
                                let conn = state.db.lock();
                                let _ = db::kv_set(
                                    &conn,
                                    "overlay_pos",
                                    &format!("[{},{}]", save_pos.0, save_pos.1),
                                );
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
            commands::overlay_click,
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
            commands::active_app_context,
            commands::ai_test,
            commands::rewrite_prompt_with_instruction,
            commands::transform_text,
            commands::default_app_profile_presets,
            commands::stt_test,
            commands::check_for_update,
            commands::install_update,
            commands::corrections_list,
            commands::corrections_upsert,
            commands::corrections_delete,
            commands::overlay_box,
            commands::overlay_hit,
        ])
        .build(tauri::generate_context!())
        .expect("error while building VoxFlow")
        .run(|app, event| match event {
            tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit => {
                request_shutdown(app);
            }
            _ => {}
        });
}

/// Показать фатальную ошибку старта понятным окном (GUI ещё не поднят, поэтому
/// нативный MessageBoxW) и продублировать в лог/стдерр. Используется до
/// инициализации Tauri — P2-7 (молчаливый краш db::open().expect).
fn fatal_startup_error(text: &str) {
    log::error!("{text}");
    eprintln!("{text}");
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "user32")]
        extern "system" {
            fn MessageBoxW(hwnd: isize, text: *const u16, caption: *const u16, utype: u32) -> i32;
        }
        let wide = |s: &str| -> Vec<u16> {
            std::ffi::OsStr::new(s)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect()
        };
        let t = wide(text);
        let c = wide("VoxFlow — ошибка запуска");
        // 0x10 = MB_ICONERROR.
        unsafe {
            MessageBoxW(0, t.as_ptr(), c.as_ptr(), 0x10);
        }
    }
}

/// Настройки для headless-селфтестов: БД открывается СТРОГО read-only.
/// Инцидент 2026-06-11: --stream-selftest через db::open() заквантинил
/// повреждённую voxflow.db (.corrupt-<ts>) и пересоздал её свежей — настройки
/// пользователя (включая API-ключ) были сброшены. Диагностика не имеет права
/// «чинить» или создавать пользовательскую БД: любая ошибка чтения (нет файла,
/// malformed, неподнимаемый WAL) → eprintln-предупреждение и Settings::default(),
/// файл остаётся нетронутым. Recovery-путь остаётся только в GUI (run() → setup).
fn cli_load_settings(tag: &str) -> settings::Settings {
    match db::open_readonly() {
        Ok(conn) => settings::load(&conn),
        Err(e) => {
            eprintln!(
                "[{tag}] БД не прочитана ({e:#}) — продолжаю на настройках по умолчанию, файл БД не трогаю"
            );
            settings::Settings::default()
        }
    }
}

/// Диагностический прогон ASR + постобработки на готовом 16 кГц WAV (без GUI/микрофона).
/// Используется как `voxflow.exe --selftest <wav>` для проверки пайплайна кодом приложения.
pub fn selftest(wav_path: &str) {
    use std::path::Path;
    let s = cli_load_settings("selftest");
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
    eprintln!(
        "[selftest] language    = {}, threads = {}",
        s.language,
        s.effective_threads()
    );

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

    let s = cli_load_settings("stream");
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
    eprintln!(
        "[stream] длительность аудио ≈ {dur_s:.2} c, сэмплов(16к)={}",
        full16.len()
    );

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
    eprintln!(
        "[stream] сервер готов за {} мс",
        t_boot.elapsed().as_millis()
    );

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

    let s = cli_load_settings("stt-test");
    eprintln!("[stt-test] provider = {}", s.stt_provider);
    eprintln!("[stt-test] language = {}", s.language);
    eprintln!(
        "[stt-test] proxy    = {}",
        net::proxy_configured(&s.proxy_url)
    );
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

/// Поллер «мышь над пилюлей»: на Windows каждые 120 мс сравнивает глобальный курсор с
/// зоной пилюли (overlay_hit от фронта, CSS px × scale + позиция окна) и
/// переключает click-through. Вне пилюли окно прозрачно для мыши — кнопки
/// фуллскрин-приложений под оверлеем остаются кликабельными. Во время зажатой
/// ЛКМ состояние не переключаем (не рвать drag пилюли). На macOS overlay
/// оставляем интерактивным всегда: системный click-through слишком легко
/// ломает реальный drag маленькой плавающей панели.
fn spawn_overlay_hover_poller(app: &tauri::AppHandle, hit: Arc<Mutex<OverlayHitRect>>) {
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
                    let Some(ov) = app.get_webview_window("overlay") else {
                        continue;
                    };
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

#[cfg(target_os = "macos")]
fn spawn_overlay_drag_poller(app: &tauri::AppHandle) {
    let app = app.clone();
    std::thread::Builder::new()
        .name("voxflow-overlay-drag".into())
        .spawn(move || {
            let mut drag: Option<((f64, f64), (i32, i32))> = None;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(16));
                let Some(ov) = app.get_webview_window("overlay") else {
                    drag = None;
                    continue;
                };
                let down = macos_left_mouse_down();
                let Some(cursor) = macos_cursor_position() else {
                    drag = None;
                    continue;
                };
                if !down {
                    drag = None;
                    continue;
                }
                if let Some((start_cursor, start_pos)) = drag {
                    let dx = cursor.0 - start_cursor.0;
                    let dy = cursor.1 - start_cursor.1;
                    if dx.abs() >= 1.0 || dy.abs() >= 1.0 {
                        let x = (start_pos.0 as f64 + dx).round() as i32;
                        let y = (start_pos.1 as f64 + dy).round() as i32;
                        let _ = ov.set_position(PhysicalPosition::new(x, y));
                    }
                    continue;
                }
                if overlay_cursor_inside_window(&ov, cursor) {
                    if let Ok(pos) = ov.outer_position() {
                        drag = Some((cursor, (pos.x, pos.y)));
                    }
                }
            }
        })
        .ok();
}

#[cfg(not(target_os = "macos"))]
fn spawn_overlay_drag_poller(_app: &tauri::AppHandle) {}

#[cfg(target_os = "macos")]
fn overlay_cursor_inside_window(ov: &tauri::WebviewWindow, cursor: (f64, f64)) -> bool {
    let (Ok(pos), Ok(size)) = (ov.outer_position(), ov.outer_size()) else {
        return false;
    };
    let x = pos.x as f64;
    let y = pos.y as f64;
    let w = size.width as f64;
    let h = size.height as f64;
    cursor.0 >= x && cursor.0 <= x + w && cursor.1 >= y && cursor.1 <= y + h
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct MacosCGPoint {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
type MacosCGEventRef = *mut std::ffi::c_void;

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreate(source: *const std::ffi::c_void) -> MacosCGEventRef;
    fn CGEventGetLocation(event: MacosCGEventRef) -> MacosCGPoint;
    fn CGEventSourceButtonState(state_id: u32, button: u32) -> bool;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

#[cfg(target_os = "macos")]
fn macos_cursor_position() -> Option<(f64, f64)> {
    let event = unsafe { CGEventCreate(std::ptr::null()) };
    if event.is_null() {
        return None;
    }
    let p = unsafe { CGEventGetLocation(event) };
    unsafe { CFRelease(event.cast()) };
    Some((p.x, p.y))
}

#[cfg(target_os = "macos")]
fn macos_left_mouse_down() -> bool {
    // 0 = kCGEventSourceStateCombinedSessionState, 0 = kCGMouseButtonLeft.
    unsafe { CGEventSourceButtonState(0, 0) }
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
    eprintln!(
        "[gigaam] загружен за {} мс ({} потоков)",
        t.elapsed().as_millis(),
        threads
    );
    let t = Instant::now();
    let _ = g.transcribe(&vec![0.0f32; 8000]);
    eprintln!("[gigaam] прогрев {} мс", t.elapsed().as_millis());

    // WAV → mono f32 16к.
    let reader = hound::WavReader::open(wav_path).expect("открыть WAV");
    let spec = reader.spec();
    let chans = spec.channels as usize;
    let raw: Vec<f32> = match spec.sample_format {
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
    let mono: Vec<f32> = if chans <= 1 {
        raw
    } else {
        raw.chunks(chans)
            .map(|f| f.iter().sum::<f32>() / f.len() as f32)
            .collect()
    };
    let full16 = audio::resample_to_16k(&mono, spec.sample_rate);
    eprintln!("[gigaam] аудио {:.2} c", full16.len() as f32 / 16000.0);

    // VAD-гейт.
    let t = Instant::now();
    let speech = vad.has_speech(&full16, 0.5).unwrap_or(true);
    eprintln!(
        "[gigaam] vad-гейт {} мс, речь={}",
        t.elapsed().as_millis(),
        speech
    );

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
        println!(
            "[p{tick:02} @ {:>4.1}c | {:>5} мс] {txt:?}",
            cut as f32 / 16000.0,
            t.elapsed().as_millis()
        );
        cut += 16000;
    }

    // Финал — тем же путём, что и боевой process_utterance: длинное аудио
    // режется по VAD-паузам на сегменты ≤25 c (engine::gigaam_transcribe_long).
    println!("──────── FINAL ────────");
    let trimmed = audio::trim_silence(&full16, 16000);
    let vad_arc = std::sync::Arc::new(parking_lot::Mutex::new(Some(vad)));
    let t = Instant::now();
    let txt = engine::local_transcribe_long(&vad_arc, &trimmed, &mut |seg| g.transcribe(seg))
        .unwrap_or_default();
    let wall = t.elapsed().as_millis();
    let st = g.last_stats;
    println!("TEXT  : {txt:?}");
    println!(
        "[lat] audio={}мс (последний сегмент: frontend={}мс encoder={}мс decoder={}мс) финал-стенка {} мс",
        trimmed.len() * 1000 / 16000, st.frontend_ms, st.encoder_ms, st.decoder_ms, wall
    );
}

/// WAV → mono f32 16 кГц (общий код headless-селфтестов Parakeet/LID).
fn read_wav_mono_16k(wav_path: &str) -> Vec<f32> {
    let reader = hound::WavReader::open(wav_path).expect("открыть WAV");
    let spec = reader.spec();
    let chans = spec.channels as usize;
    let raw: Vec<f32> = match spec.sample_format {
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
    let mono: Vec<f32> = if chans <= 1 {
        raw
    } else {
        raw.chunks(chans)
            .map(|f| f.iter().sum::<f32>() / f.len() as f32)
            .collect()
    };
    audio::resample_to_16k(&mono, spec.sample_rate)
}

fn request_shutdown(app: &tauri::AppHandle) {
    static SHUTDOWN_SENT: AtomicBool = AtomicBool::new(false);
    if SHUTDOWN_SENT.swap(true, Ordering::SeqCst) {
        return;
    }
    if let Some(state) = app.try_state::<AppState>() {
        state.engine.restore_auto_mute();
        let _ = state.engine_tx.lock().send(EngineCmd::Shutdown);
    }
}

/// Headless-проверка Parakeet TDT v3 (без GUI/микрофона): тайминги load/warmup,
/// транскрипт и раскладка инференса по этапам — по образцу --gigaam-selftest.
/// Запуск: `voxflow.exe --parakeet-selftest <wav>`.
pub fn parakeet_selftest(wav_path: &str) {
    use std::time::Instant;

    let dir = paths::parakeet_dir();
    eprintln!("[parakeet] модели: {dir:?}");
    if !parakeet::dir_ready(&dir) {
        eprintln!("[parakeet] ОШИБКА: модель не установлена (вкладка «Модель» → Parakeet TDT v3)");
        return;
    }
    let threads = settings::Settings::default().effective_threads() as usize;
    let t = Instant::now();
    let mut p = match parakeet::Parakeet::load(&dir, threads) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[parakeet] ОШИБКА загрузки: {e:#}");
            return;
        }
    };
    let load_ms = t.elapsed().as_millis();
    eprintln!("[parakeet] загружен за {load_ms} мс ({threads} потоков)");
    let t = Instant::now();
    let _ = p.transcribe(&vec![0.0f32; 8000]);
    let warm_ms = t.elapsed().as_millis();
    eprintln!("[parakeet] прогрев {warm_ms} мс");

    let full16 = read_wav_mono_16k(wav_path);
    eprintln!("[parakeet] аудио {:.2} c", full16.len() as f32 / 16000.0);
    let trimmed = audio::trim_silence(&full16, 16000);
    let t = Instant::now();
    let txt = p.transcribe(&trimmed).unwrap_or_default();
    let wall = t.elapsed().as_millis();
    let st = p.last_stats;
    println!("TEXT  : {txt:?}");
    println!(
        "[lat] load={load_ms}мс warmup={warm_ms}мс audio={}мс frontend={}мс encoder={}мс decoder={}мс infer-стенка {wall} мс",
        st.audio_ms, st.frontend_ms, st.encoder_ms, st.decoder_ms
    );
}

/// Headless-проверка LID-роутера (language="auto"): Parakeet транскрибирует,
/// скрипт текста определяет язык; кириллица → перегон через GigaAM — ровно как
/// auto-маршрут финала. Печатает определённый язык, выбранный маршрут и текст.
/// Запуск: `voxflow.exe --lid-selftest <wav>`.
pub fn lid_selftest(wav_path: &str) {
    use std::time::Instant;

    let pdir = paths::parakeet_dir();
    if !parakeet::dir_ready(&pdir) {
        eprintln!("[lid] ОШИБКА: модель Parakeet не установлена — auto-роутер недоступен");
        return;
    }
    let threads = settings::Settings::default().effective_threads() as usize;
    let mut p = match parakeet::Parakeet::load(&pdir, threads) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[lid] parakeet ОШИБКА загрузки: {e:#}");
            return;
        }
    };
    let _ = p.transcribe(&vec![0.0f32; 8000]); // прогрев

    let full16 = read_wav_mono_16k(wav_path);
    let trimmed = audio::trim_silence(&full16, 16000);
    eprintln!("[lid] аудио {:.2} c", trimmed.len() as f32 / 16000.0);
    let t = Instant::now();
    let draft = p.transcribe(&trimmed).unwrap_or_default();
    let p_ms = t.elapsed().as_millis();
    let cyr = parakeet::is_mostly_cyrillic(&draft);
    let lang = if cyr {
        "ru"
    } else if draft.chars().any(|c| c.is_ascii_alphabetic()) {
        "en"
    } else {
        "??"
    };
    println!("LANG  : {lang}");
    println!("DRAFT : {draft:?} ({p_ms} мс, parakeet)");
    if cyr && gigaam::dir_ready(&paths::gigaam_dir()) {
        let mut g = match gigaam::GigaAm::load(&paths::gigaam_dir(), threads) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("[lid] gigaam ОШИБКА загрузки: {e:#}");
                println!("ROUTE : parakeet (gigaam не загрузился)");
                println!("TEXT  : {draft:?}");
                return;
            }
        };
        let _ = g.transcribe(&vec![0.0f32; 8000]); // прогрев
        let t = Instant::now();
        let txt = g.transcribe(&trimmed).unwrap_or_default();
        println!("ROUTE : parakeet → gigaam (кириллический скрипт)");
        println!("TEXT  : {txt:?} ({} мс, gigaam)", t.elapsed().as_millis());
    } else {
        println!("ROUTE : parakeet");
        println!("TEXT  : {draft:?}");
    }
}

fn build_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    use tauri::menu::{CheckMenuItem, Submenu};

    let settings_i = MenuItem::with_id(app, "settings", "Настройки", true, None::<&str>)?;
    let toggle_i = MenuItem::with_id(app, "toggle", "Старт / стоп диктовки", true, None::<&str>)?;
    let quit_i = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    // Подменю «Язык» — быстрое переключение Авто/RU/EN без открытия настроек.
    // Начальное состояние галок — из сохранённых настроек (AppState уже managed).
    let lang0 = app
        .try_state::<AppState>()
        .map(|st| st.settings.lock().language.clone())
        .unwrap_or_else(|| "ru".into());
    let lang_auto = CheckMenuItem::with_id(
        app,
        "lang_auto",
        "Авто",
        true,
        lang0 == "auto",
        None::<&str>,
    )?;
    let lang_ru =
        CheckMenuItem::with_id(app, "lang_ru", "Русский", true, lang0 == "ru", None::<&str>)?;
    let lang_en =
        CheckMenuItem::with_id(app, "lang_en", "English", true, lang0 == "en", None::<&str>)?;
    let lang_sub = Submenu::with_items(app, "Язык", true, &[&lang_auto, &lang_ru, &lang_en])?;

    // Клоны итемов — в AppState: save_settings синхронизирует галки и при смене
    // языка из UI, и при клике в трее (единая точка синхронизации).
    if let Some(state) = app.try_state::<AppState>() {
        *state.lang_menu.lock() = Some(commands::LangMenu {
            auto: lang_auto.clone(),
            ru: lang_ru.clone(),
            en: lang_en.clone(),
        });
    }

    let menu = Menu::with_items(app, &[&settings_i, &toggle_i, &lang_sub, &sep, &quit_i])?;

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
            id @ ("lang_auto" | "lang_ru" | "lang_en") => {
                let lang = match id {
                    "lang_ru" => "ru",
                    "lang_en" => "en",
                    _ => "auto",
                };
                if let Some(state) = app.try_state::<AppState>() {
                    // Тот же путь, что и сохранение из UI (commands::save_settings):
                    // БД → снимок в памяти → автозапуск → синхронизация галок трея.
                    let mut s = state.settings.lock().clone();
                    s.language = lang.to_string();
                    if let Err(e) = commands::save_settings(app.clone(), state, s) {
                        log::error!("трей: смена языка не сохранилась: {e}");
                    }
                }
            }
            "quit" => {
                request_shutdown(app);
                app.exit(0);
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

/// Overlay-индикатор (orb): маленькое плавающее окно. На macOS оно всегда
/// интерактивно в пределах своего небольшого бокса, чтобы drag не ломался
/// из-за системного click-through. На Windows hover-poller по-прежнему
/// включает мышь только над актуальным hit-rect.
/// Позиция запоминается в kv "overlay_pos" и восстанавливается на старте.
fn setup_overlay(app: &tauri::AppHandle) {
    if let Some(ov) = app.get_webview_window("overlay") {
        // На macOS настоящие mouse-drag события должны доходить до webview.
        // Windows сохраняет старую click-through модель через hover-poller.
        #[cfg(target_os = "macos")]
        let _ = ov.set_ignore_cursor_events(false);
        #[cfg(not(target_os = "macos"))]
        let _ = ov.set_ignore_cursor_events(true);
        let _ = ov.set_always_on_top(true);
        let scale = ov.scale_factor().unwrap_or(1.0);
        // Стартовый размер = idle-запрос фронта (220×80: запас под hover-рост
        // и тултип); дальше фронт сам зовёт overlay_box на каждом состоянии.
        let win_w = 220.0 * scale;
        let win_h = 80.0 * scale;
        let _ = ov.set_size(PhysicalSize::new(win_w as u32, win_h as u32));
        // Сохранённая позиция, если она остаётся видимой на одном из экранов;
        // иначе низ-центр. На macOS координаты могут быть глобальными для
        // нескольких мониторов, поэтому нельзя валидировать только через
        // primary_monitor().size().
        let saved: Option<(i32, i32)> = {
            let state = app.state::<AppState>();
            let conn = state.db.lock();
            db::kv_get(&conn, "overlay_pos").and_then(|j| serde_json::from_str(&j).ok())
        };
        let mut placed = false;
        if let (Some((x, y)), Ok(monitors)) = (saved, ov.available_monitors()) {
            let win_w_i = win_w.round() as i32;
            let win_h_i = win_h.round() as i32;
            let visible = monitors.iter().any(|mon| {
                let area = mon.work_area();
                overlay_position_visible(
                    (x, y),
                    (win_w_i, win_h_i),
                    (
                        area.position.x,
                        area.position.y,
                        area.size.width as i32,
                        area.size.height as i32,
                    ),
                )
            });
            if visible {
                let _ = ov.set_position(PhysicalPosition::new(x, y));
                placed = true;
            }
        }
        if !placed {
            if let Ok(Some(mon)) = ov.primary_monitor() {
                let area = mon.work_area();
                let x = area.position.x + ((area.size.width as f64 - win_w) / 2.0).max(0.0) as i32;
                // Держим заметно выше Dock/menu-safe area: на macOS маленькая
                // recording-пилюля иначе визуально терялась у нижней кромки.
                let y = area.position.y
                    + (area.size.height as f64 - win_h - 156.0 * scale).max(0.0) as i32;
                let _ = ov.set_position(PhysicalPosition::new(x, y));
            }
        }
        let _ = ov.show();
    }
}

fn overlay_position_visible(
    position: (i32, i32),
    window_size: (i32, i32),
    work_area: (i32, i32, i32, i32),
) -> bool {
    const MIN_VISIBLE: i64 = 48;
    const MIN_BOTTOM_GAP: i64 = 96;
    let (x, y) = position;
    let (win_w, win_h) = window_size;
    let (area_x, area_y, area_w, area_h) = work_area;
    let (x, y, win_w, win_h) = (x as i64, y as i64, win_w as i64, win_h as i64);
    let (area_x, area_y, area_w, area_h) =
        (area_x as i64, area_y as i64, area_w as i64, area_h as i64);
    let area_right = area_x + area_w;
    let area_bottom = area_y + area_h;

    x + win_w - area_x >= MIN_VISIBLE
        && y + win_h - area_y >= MIN_VISIBLE
        && area_right - x >= MIN_VISIBLE
        && area_bottom - y >= MIN_VISIBLE
        && area_bottom - (y + win_h) >= MIN_BOTTOM_GAP
}

fn overlay_snap_position(
    position: (i32, i32),
    window_size: (i32, i32),
    work_areas: &[(i32, i32, i32, i32)],
    scale_factor: f64,
) -> Option<(i32, i32)> {
    let work_area = overlay_best_work_area(position, window_size, work_areas)?;
    Some(overlay_snap_position_in_work_area(
        position,
        window_size,
        work_area,
        scale_factor,
    ))
}

fn overlay_best_work_area(
    position: (i32, i32),
    window_size: (i32, i32),
    work_areas: &[(i32, i32, i32, i32)],
) -> Option<(i32, i32, i32, i32)> {
    let (x, y) = position;
    let (win_w, win_h) = window_size;
    let center = (x as i64 + win_w as i64 / 2, y as i64 + win_h as i64 / 2);

    work_areas
        .iter()
        .copied()
        .min_by_key(|&(area_x, area_y, area_w, area_h)| {
            let left = area_x as i64;
            let top = area_y as i64;
            let right = left + area_w as i64;
            let bottom = top + area_h as i64;
            let dx = if center.0 < left {
                left - center.0
            } else if center.0 > right {
                center.0 - right
            } else {
                0
            };
            let dy = if center.1 < top {
                top - center.1
            } else if center.1 > bottom {
                center.1 - bottom
            } else {
                0
            };
            dx * dx + dy * dy
        })
}

fn overlay_snap_position_in_work_area(
    position: (i32, i32),
    window_size: (i32, i32),
    work_area: (i32, i32, i32, i32),
    scale_factor: f64,
) -> (i32, i32) {
    let (x, y) = position;
    let (win_w, win_h) = window_size;
    let (area_x, area_y, area_w, area_h) = work_area;
    let scale = scale_factor.clamp(1.0, 3.0);
    let side_gap = ((16.0 * scale).round() as i32).max(14);
    let bottom_gap = ((56.0 * scale).round() as i32).max(96);
    let y_threshold = ((64.0 * scale).round() as i32).max(70);

    let max_x = area_x + area_w - win_w - side_gap;
    let left_x = area_x + side_gap;
    let center_x = area_x + ((area_w - win_w) / 2).max(0);
    let right_x = max_x.max(left_x);
    let snapped_x = [left_x, center_x, right_x]
        .into_iter()
        .min_by_key(|candidate| (x - *candidate).abs())
        .unwrap_or(center_x);

    let top_y = area_y + side_gap;
    let bottom_y = (area_y + area_h - win_h - bottom_gap).max(top_y);
    let center_y = area_y + ((area_h - win_h) / 2).max(0);
    let mut snapped_y = y.clamp(top_y, bottom_y);
    if let Some(anchor) = [top_y, center_y, bottom_y]
        .into_iter()
        .min_by_key(|candidate| (y - *candidate).abs())
    {
        if (y - anchor).abs() <= y_threshold {
            snapped_y = anchor.clamp(top_y, bottom_y);
        }
    }

    (snapped_x, snapped_y)
}

#[cfg(test)]
mod lib_tests {
    use super::{
        overlay_best_work_area, overlay_position_visible, overlay_snap_position,
        overlay_snap_position_in_work_area,
    };

    #[test]
    fn overlay_position_accepts_visible_saved_position() {
        assert!(overlay_position_visible(
            (1200, 800),
            (220, 80),
            (0, 0, 3000, 1700)
        ));
    }

    #[test]
    fn overlay_position_rejects_mostly_offscreen_saved_position() {
        assert!(!overlay_position_visible(
            (1700, 2198),
            (220, 80),
            (0, 0, 3000, 1700)
        ));
        assert!(!overlay_position_visible(
            (-210, 100),
            (220, 80),
            (0, 0, 3000, 1700)
        ));
        assert!(!overlay_position_visible(
            (900, 1131),
            (220, 80),
            (0, 0, 1976, 1211)
        ));
    }

    #[test]
    fn overlay_position_accepts_secondary_monitor_coordinates() {
        assert!(overlay_position_visible(
            (3300, 600),
            (220, 80),
            (3000, 0, 1920, 1080)
        ));
    }

    #[test]
    fn overlay_snap_uses_left_center_right_anchors() {
        let area = (0, 0, 3000, 1700);
        assert_eq!(
            overlay_snap_position_in_work_area((40, 800), (220, 80), area, 2.0).0,
            32
        );
        assert_eq!(
            overlay_snap_position_in_work_area((1370, 800), (220, 80), area, 2.0).0,
            1390
        );
        assert_eq!(
            overlay_snap_position_in_work_area((2740, 800), (220, 80), area, 2.0).0,
            2748
        );
    }

    #[test]
    fn overlay_snap_preserves_free_vertical_position_away_from_anchors() {
        let snapped =
            overlay_snap_position_in_work_area((40, 430), (220, 80), (0, 0, 3000, 1700), 2.0);
        assert_eq!(snapped, (32, 430));
    }

    #[test]
    fn overlay_snap_magnets_vertical_edges_when_nearby() {
        let area = (0, 0, 3000, 1700);
        assert_eq!(
            overlay_snap_position_in_work_area((40, 26), (220, 80), area, 2.0),
            (32, 32)
        );
        assert_eq!(
            overlay_snap_position_in_work_area((40, 1488), (220, 80), area, 2.0),
            (32, 1508)
        );
    }

    #[test]
    fn overlay_snap_selects_nearest_monitor() {
        let monitors = [(0, 0, 3000, 1700), (3000, 0, 1920, 1080)];
        assert_eq!(
            overlay_best_work_area((3300, 600), (220, 80), &monitors),
            Some((3000, 0, 1920, 1080))
        );
        assert_eq!(
            overlay_snap_position((3300, 600), (220, 80), &monitors, 2.0),
            Some((3032, 500))
        );
    }
}
