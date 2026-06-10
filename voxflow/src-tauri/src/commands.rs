//! IPC-команды для фронтенда + общее состояние приложения.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::engine::EngineCmd;
use crate::models;
use crate::settings::{self, Settings};

/// Состояние, разделяемое между командами, движком и хоткеем.
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub settings: Arc<Mutex<Settings>>,
    pub engine_tx: Mutex<Sender<EngineCmd>>,
    pub recording: Arc<AtomicBool>,
    /// Прямоугольник пилюли внутри overlay-окна (CSS px: x,y,w,h) — зона, где
    /// окно должно ловить мышь. Вне её окно click-through (фуллскрин-приложения
    /// под оверлеем остаются кликабельными). Обновляет фронт (overlay_hit).
    pub overlay_hit: Arc<Mutex<Option<(f64, f64, f64, f64)>>>,
}

type R<T> = Result<T, String>;
fn err<E: std::fmt::Display>(x: E) -> String {
    x.to_string()
}

// ─────────────────────────── Настройки ───────────────────────────

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().clone()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, state: State<AppState>, settings: Settings) -> R<()> {
    apply_autostart(&app, settings.autostart);
    // Сначала ПИШЕМ в БД и только при успехе обновляем снимок в памяти — чтобы
    // провал записи был виден во фронте (B4), а не проглатывался молча.
    {
        let conn = state.db.lock();
        settings::save(&conn, &settings).map_err(err)?;
    }
    *state.settings.lock() = settings;
    Ok(())
}

fn apply_autostart(app: &AppHandle, on: bool) {
    use tauri_plugin_autostart::ManagerExt;
    let m = app.autolaunch();
    match if on { m.enable() } else { m.disable() } {
        Ok(_) => log::info!("autostart set to {on}"),
        Err(e) => log::error!("autostart set {on} failed: {e}"),
    }
}

// ─────────────────────────── Аудио / модели ───────────────────────────

#[tauri::command]
pub fn list_audio_devices() -> Vec<String> {
    crate::audio::list_input_devices()
}

#[tauri::command]
pub fn list_models() -> Vec<models::ModelInfo> {
    models::list()
}

#[tauri::command]
pub fn download_model(app: AppHandle, name: String) -> R<()> {
    models::start_download(app, name).map_err(err)
}

#[tauri::command]
pub fn delete_model(name: String) -> R<()> {
    models::delete(&name).map_err(err)
}

// ─────────────────────────── Диктовка ───────────────────────────

#[tauri::command]
pub fn toggle_dictation(state: State<AppState>) -> R<()> {
    state.engine_tx.lock().send(EngineCmd::Toggle).map_err(err)
}

#[tauri::command]
pub fn is_recording(state: State<AppState>) -> bool {
    state.recording.load(std::sync::atomic::Ordering::SeqCst)
}

// ─────────────────────────── Статистика / история ───────────────────────────

#[derive(Serialize)]
pub struct Stats {
    total_words: i64,
    total_sessions: i64,
    today_words: i64,
    streak_days: i64,
    apps_count: i64,
}

#[tauri::command]
pub fn get_stats(state: State<AppState>) -> Stats {
    let conn = state.db.lock();
    let total_words = conn
        .query_row("SELECT COALESCE(SUM(words),0) FROM stats", [], |r| r.get(0))
        .unwrap_or(0);
    let total_sessions = conn
        .query_row("SELECT COALESCE(SUM(sessions),0) FROM stats", [], |r| r.get(0))
        .unwrap_or(0);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_words = conn
        .query_row("SELECT COALESCE(words,0) FROM stats WHERE day=?1", [today], |r| r.get(0))
        .unwrap_or(0);
    let apps_count = conn
        .query_row("SELECT COUNT(DISTINCT app) FROM history WHERE app<>''", [], |r| r.get(0))
        .unwrap_or(0);
    let streak_days = compute_streak(&conn);
    Stats { total_words, total_sessions, today_words, streak_days, apps_count }
}

fn compute_streak(conn: &Connection) -> i64 {
    let mut streak = 0i64;
    let mut day = chrono::Local::now().date_naive();
    loop {
        let ds = day.format("%Y-%m-%d").to_string();
        let cnt: i64 = conn
            .query_row("SELECT COALESCE(sessions,0) FROM stats WHERE day=?1", [ds], |r| r.get(0))
            .unwrap_or(0);
        if cnt > 0 {
            streak += 1;
            match day.pred_opt() {
                Some(d) => day = d,
                None => break,
            }
            if streak > 3650 {
                break;
            }
        } else {
            break;
        }
    }
    streak
}

#[derive(Serialize)]
pub struct HistoryItem {
    ts: String,
    text: String,
    app: String,
    words: i64,
}

#[tauri::command]
pub fn get_history(state: State<AppState>, limit: u32) -> Vec<HistoryItem> {
    let conn = state.db.lock();
    let mut out = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT ts,text,app,words FROM history ORDER BY id DESC LIMIT ?1")
    {
        if let Ok(rows) = stmt.query_map([limit], |r| {
            Ok(HistoryItem {
                ts: r.get(0)?,
                text: r.get(1)?,
                app: r.get(2)?,
                words: r.get(3)?,
            })
        }) {
            out.extend(rows.flatten());
        }
    }
    out
}

// ─────────────────────────── Словарь ───────────────────────────

#[derive(Serialize)]
pub struct DictItem {
    id: i64,
    term: String,
    replacement: String,
}

#[tauri::command]
pub fn dictionary_list(state: State<AppState>) -> Vec<DictItem> {
    let conn = state.db.lock();
    let mut out = Vec::new();
    if let Ok(mut stmt) = conn.prepare("SELECT id,term,replacement FROM dictionary ORDER BY id") {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(DictItem {
                id: r.get(0)?,
                term: r.get(1)?,
                replacement: r.get(2)?,
            })
        }) {
            out.extend(rows.flatten());
        }
    }
    out
}

#[tauri::command]
pub fn dictionary_upsert(
    state: State<AppState>,
    id: Option<i64>,
    term: String,
    replacement: String,
) -> R<()> {
    let conn = state.db.lock();
    match id {
        Some(i) => conn.execute(
            "UPDATE dictionary SET term=?1, replacement=?2 WHERE id=?3",
            params![term, replacement, i],
        ),
        None => conn.execute(
            "INSERT INTO dictionary(term,replacement) VALUES(?1,?2)",
            params![term, replacement],
        ),
    }
    .map_err(err)?;
    Ok(())
}

#[tauri::command]
pub fn dictionary_delete(state: State<AppState>, id: i64) -> R<()> {
    state
        .db
        .lock()
        .execute("DELETE FROM dictionary WHERE id=?1", [id])
        .map_err(err)?;
    Ok(())
}

// ─────────────────────────── Сниппеты ───────────────────────────

#[derive(Serialize)]
pub struct SnippetItem {
    id: i64,
    trigger: String,
    content: String,
    is_template: bool,
}

#[tauri::command]
pub fn snippet_list(state: State<AppState>) -> Vec<SnippetItem> {
    let conn = state.db.lock();
    let mut out = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT id,trigger,content,is_template FROM snippets ORDER BY id")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            let is_t: i64 = r.get(3)?;
            Ok(SnippetItem {
                id: r.get(0)?,
                trigger: r.get(1)?,
                content: r.get(2)?,
                is_template: is_t != 0,
            })
        }) {
            out.extend(rows.flatten());
        }
    }
    out
}

#[tauri::command]
pub fn snippet_upsert(
    state: State<AppState>,
    id: Option<i64>,
    trigger: String,
    content: String,
    is_template: bool,
) -> R<()> {
    let conn = state.db.lock();
    let tflag = if is_template { 1i64 } else { 0 };
    match id {
        Some(i) => conn.execute(
            "UPDATE snippets SET trigger=?1, content=?2, is_template=?3 WHERE id=?4",
            params![trigger, content, tflag, i],
        ),
        None => conn.execute(
            "INSERT INTO snippets(trigger,content,is_template) VALUES(?1,?2,?3)
             ON CONFLICT(trigger) DO UPDATE SET content=excluded.content, is_template=excluded.is_template",
            params![trigger, content, tflag],
        ),
    }
    .map_err(err)?;
    Ok(())
}

#[tauri::command]
pub fn snippet_delete(state: State<AppState>, id: i64) -> R<()> {
    state
        .db
        .lock()
        .execute("DELETE FROM snippets WHERE id=?1", [id])
        .map_err(err)?;
    Ok(())
}

// ─────────────────────────── Окно ───────────────────────────

#[tauri::command]
pub fn show_main_window(app: AppHandle) -> R<()> {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
    Ok(())
}

// ─────────────────────────── ИИ / исправления ───────────────────────────

#[derive(Serialize)]
pub struct AiTestResult {
    ok: bool,
    message: String,
}

#[tauri::command]
pub fn ai_test(state: State<AppState>) -> AiTestResult {
    let (backend, key, model, ollama_url, ollama_model) = {
        let s = state.settings.lock();
        (
            s.ai_backend.clone(),
            s.ai_api_key.clone(),
            s.ai_model.clone(),
            s.ollama_url.clone(),
            s.ollama_model.clone(),
        )
    };
    match backend.as_str() {
        "gemini" => {
            if key.trim().is_empty() {
                return AiTestResult { ok: false, message: "Введите API-ключ".into() };
            }
            match crate::gemini::refine(&key, &model, "Ответь ровно одним словом.", "Напиши: ОК") {
                Ok(t) => AiTestResult { ok: true, message: format!("Gemini отвечает: {}", t.trim()) },
                Err(e) => AiTestResult { ok: false, message: format!("Ошибка: {e}") },
            }
        }
        "ollama" => {
            // Сначала проверяем, что Ollama запущена и нужная модель скачана,
            // затем делаем «ОК»-пробу. Имена моделей бывают с тегом ("qwen3:4b"),
            // поэтому принимаем и точное совпадение, и префикс с двоеточием.
            match crate::ollama::list_models(&ollama_url) {
                Err(e) => AiTestResult {
                    ok: false,
                    message: format!("Ollama не запущена ({ollama_url}). {e}"),
                },
                Ok(models)
                    if !models.iter().any(|m| {
                        m == &ollama_model || m.starts_with(&format!("{ollama_model}:"))
                    }) =>
                {
                    AiTestResult {
                        ok: false,
                        message: format!(
                            "Модель '{ollama_model}' не найдена. Скачайте: ollama pull {ollama_model}"
                        ),
                    }
                }
                Ok(_) => match crate::ollama::refine(
                    &ollama_url,
                    &ollama_model,
                    "Ответь ровно одним словом.",
                    "Напиши: ОК",
                ) {
                    Ok(t) => AiTestResult {
                        ok: true,
                        message: format!("Ollama отвечает: {}", t.trim()),
                    },
                    Err(e) => AiTestResult { ok: false, message: format!("Ошибка: {e}") },
                },
            }
        }
        _ => AiTestResult { ok: false, message: "Движок ИИ выключен".into() },
    }
}

/// Проба облачного STT: шлёт короткий WAV-тишины (0.4 c) выбранному провайдеру и
/// возвращает человеко-читаемый русский результат. Ключ нигде не печатается.
///
/// Мьютекс настроек НЕ удерживается через сетевой вызов: снимаем клон под локом,
/// затем отпускаем — curl ходит уже без блокировки общего состояния.
#[tauri::command]
pub async fn stt_test(state: State<'_, AppState>) -> Result<String, String> {
    let s = state.settings.lock().clone();

    // Локальный провайдер облачную пробу не имеет смысла — сообщаем явно.
    if s.stt_provider != "openai_compat" && s.stt_provider != "deepgram" {
        return Ok(format!("провайдер «{}» — не облачный (нечего проверять)", s.stt_provider));
    }

    // Маленький тестовый WAV: 0.4 c тишины (16к * 0.4 = 6400 сэмплов).
    let wav = crate::paths::tmp_dir().join("stt_test.wav");
    if let Err(e) = crate::audio::write_wav_16k_mono(&wav, &vec![0.0f32; 6400]) {
        return Err(format!("не удалось создать тестовый WAV: {e}"));
    }

    let provider = s.stt_provider.clone();
    match crate::cloud_stt::transcribe(&s, &wav) {
        Ok(_) => Ok(format!("ок: {provider} ответил")),
        Err(e) => {
            // Классифицируем ошибку в русскую строку, БЕЗ ключа в тексте.
            let msg = e.to_string();
            let low = msg.to_lowercase();
            if low.contains("ключ не задан") {
                Ok("ключ не задан".into())
            } else if low.contains("401")
                || low.contains("403")
                || low.contains("unauthorized")
                || low.contains("forbidden")
                || low.contains("invalid_auth")
                || low.contains("invalid api")
                || low.contains("invalid_api")
                || low.contains("incorrect api")
                || low.contains("api key")
                || low.contains("credential")
                || low.contains("authentication")
            {
                Ok("неверный ключ (401/403)".into())
            } else {
                // Прочее (сеть/прокси/таймаут/не-JSON) — единый человекочитаемый исход.
                Ok(format!("сеть/прокси недоступны: {msg}"))
            }
        }
    }
}

#[derive(Serialize)]
pub struct CorrectionItem {
    id: i64,
    wrong: String,
    right: String,
}

#[tauri::command]
pub fn corrections_list(state: State<AppState>) -> Vec<CorrectionItem> {
    let conn = state.db.lock();
    let mut out = Vec::new();
    if let Ok(mut stmt) =
        conn.prepare("SELECT id,wrong,right FROM corrections ORDER BY hits DESC, id DESC")
    {
        if let Ok(rows) = stmt.query_map([], |r| {
            Ok(CorrectionItem {
                id: r.get(0)?,
                wrong: r.get(1)?,
                right: r.get(2)?,
            })
        }) {
            out.extend(rows.flatten());
        }
    }
    out
}

#[tauri::command]
pub fn corrections_upsert(
    state: State<AppState>,
    id: Option<i64>,
    wrong: String,
    right: String,
) -> R<()> {
    let conn = state.db.lock();
    match id {
        Some(i) => conn.execute(
            "UPDATE corrections SET wrong=?1, right=?2 WHERE id=?3",
            params![wrong, right, i],
        ),
        None => conn.execute(
            "INSERT INTO corrections(wrong,right) VALUES(?1,?2)",
            params![wrong, right],
        ),
    }
    .map_err(err)?;
    Ok(())
}

#[tauri::command]
pub fn corrections_delete(state: State<AppState>, id: i64) -> R<()> {
    state
        .db
        .lock()
        .execute("DELETE FROM corrections WHERE id=?1", [id])
        .map_err(err)?;
    Ok(())
}

/// Подогнать окно overlay под текущий размер пилюли (логические px от фронта).
/// Низ окна держим на месте (пилюля растёт вверх), X — по центру прежнего окна,
/// чтобы перетащенная пользователем позиция не сбрасывалась при смене состояния.
#[tauri::command]
pub fn overlay_box(app: AppHandle, w: f64, h: f64) -> R<()> {
    use tauri::{PhysicalPosition, PhysicalSize};
    let Some(ov) = app.get_webview_window("overlay") else {
        return Ok(());
    };
    let scale = ov.scale_factor().unwrap_or(1.0);
    let new_w = (w * scale).round().max(1.0) as i32;
    let new_h = (h * scale).round().max(1.0) as i32;
    let (old_pos, old_size) = match (ov.outer_position(), ov.outer_size()) {
        (Ok(p), Ok(s)) => (p, s),
        _ => return Ok(()),
    };
    if old_size.width as i32 == new_w && old_size.height as i32 == new_h {
        return Ok(());
    }
    let x = old_pos.x + (old_size.width as i32 - new_w) / 2;
    let y = old_pos.y + (old_size.height as i32 - new_h);
    let _ = ov.set_size(PhysicalSize::new(new_w as u32, new_h as u32));
    let _ = ov.set_position(PhysicalPosition::new(x, y));
    Ok(())
}


/// Фронт оверлея сообщает прямоугольник пилюли (CSS px относительно вьюпорта
/// окна). Поллер курсора в lib.rs включает мышь только над этой зоной.
#[tauri::command]
pub fn overlay_hit(state: State<AppState>, x: f64, y: f64, w: f64, h: f64) -> R<()> {
    *state.overlay_hit.lock() = Some((x, y, w, h));
    Ok(())
}
