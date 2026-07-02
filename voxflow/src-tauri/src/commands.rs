//! IPC-команды для фронтенда + общее состояние приложения.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{params, Connection};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::engine::{EngineCmd, EngineHandle};
use crate::models;
use crate::settings::{self, ProfileOverride, Settings};

/// Чек-итемы подменю «Язык» в трее (Авто/Русский/English). Клоны живут здесь,
/// чтобы save_settings синхронизировал галки при ЛЮБОМ источнике смены языка
/// (UI и трей идут через один путь). tauri 2 делегирует мутации меню на главный
/// поток — хранить и дёргать из команд безопасно.
pub struct LangMenu {
    pub auto: tauri::menu::CheckMenuItem<tauri::Wry>,
    pub ru: tauri::menu::CheckMenuItem<tauri::Wry>,
    pub en: tauri::menu::CheckMenuItem<tauri::Wry>,
}

impl LangMenu {
    /// Привести галки к актуальному языку настроек ("auto" | "ru" | "en").
    pub fn sync(&self, lang: &str) {
        let _ = self.auto.set_checked(lang == "auto");
        let _ = self.ru.set_checked(lang == "ru");
        let _ = self.en.set_checked(lang == "en");
    }
}

pub type OverlayHitRect = Option<(f64, f64, f64, f64)>;

/// Состояние, разделяемое между командами, движком и хоткеем.
pub struct AppState {
    pub db: Arc<Mutex<Connection>>,
    pub settings: Arc<Mutex<Settings>>,
    pub engine: EngineHandle,
    pub engine_tx: Mutex<Sender<EngineCmd>>,
    pub recording: Arc<AtomicBool>,
    /// Прямоугольник пилюли внутри overlay-окна (CSS px: x,y,w,h) — зона, где
    /// окно должно ловить мышь. Вне её окно click-through (фуллскрин-приложения
    /// под оверлеем остаются кликабельными). Обновляет фронт (overlay_hit).
    pub overlay_hit: Arc<Mutex<OverlayHitRect>>,
    /// Подменю «Язык» трея; None до build_tray. Синхронизируется в save_settings.
    pub lang_menu: Mutex<Option<LangMenu>>,
}

type R<T> = Result<T, String>;
fn err<E: std::fmt::Display>(x: E) -> String {
    x.to_string()
}

// ─────────────────────────── Настройки ───────────────────────────

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> Settings {
    state.settings.lock().clone().redacted_for_renderer()
}

#[tauri::command]
pub fn save_settings(app: AppHandle, state: State<AppState>, mut settings: Settings) -> R<()> {
    let previous = state.settings.lock().clone();
    settings.preserve_empty_secrets_from(&previous);
    apply_autostart(&app, settings.autostart);
    // Сначала ПИШЕМ в БД и только при успехе обновляем снимок в памяти — чтобы
    // провал записи был виден во фронте (B4), а не проглатывался молча.
    {
        let conn = state.db.lock();
        settings::save(&conn, &settings).map_err(err)?;
    }
    // Галки подменю «Язык» в трее — единая точка синхронизации (и UI, и трей
    // проходят через save_settings).
    if let Some(menu) = state.lang_menu.lock().as_ref() {
        menu.sync(&settings.language);
    }
    // Смену языка фиксируем при подмене снимка в памяти — по ней ниже греем движок.
    let lang_changed = {
        let mut cur = state.settings.lock();
        let changed = cur.language != settings.language;
        *cur = settings.clone();
        changed
    };
    // Рассылаем актуальные настройки всем окнам (settings_changed): главное окно
    // живёт спрятанным (hide, React смонтирован) с устаревшим снапшотом и без
    // этого события откатывало бы смену языка из трея своим безусловным flush'ем
    // на visibilitychange (lost update).
    if let Err(e) = app.emit("settings_changed", settings.clone().redacted_for_renderer()) {
        log::warn!("settings_changed не разослалось: {e}");
    }
    // Язык сменился → фоновый прогрев движка под новые настройки: без него первый
    // Start после переключения на en/auto синхронно грузит ~650 МБ Parakeet и
    // подвешивает начало диктовки на секунды.
    if lang_changed {
        if let Err(e) = state.engine_tx.lock().send(EngineCmd::Warmup) {
            log::warn!("warmup после смены языка не отправился: {e}");
        }
    }
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
        .query_row("SELECT COALESCE(SUM(sessions),0) FROM stats", [], |r| {
            r.get(0)
        })
        .unwrap_or(0);
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let today_words = conn
        .query_row(
            "SELECT COALESCE(words,0) FROM stats WHERE day=?1",
            [today],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let apps_count = conn
        .query_row(
            "SELECT COUNT(DISTINCT app) FROM history WHERE app<>''",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let streak_days = compute_streak(&conn);
    Stats {
        total_words,
        total_sessions,
        today_words,
        streak_days,
        apps_count,
    }
}

fn compute_streak(conn: &Connection) -> i64 {
    let mut streak = 0i64;
    let mut day = chrono::Local::now().date_naive();
    loop {
        let ds = day.format("%Y-%m-%d").to_string();
        let cnt: i64 = conn
            .query_row(
                "SELECT COALESCE(sessions,0) FROM stats WHERE day=?1",
                [ds],
                |r| r.get(0),
            )
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
pub struct ActiveAppContext {
    exe: String,
    title: String,
    profile: String,
    builtin_profile: String,
}

#[tauri::command]
pub fn active_app_context(state: State<AppState>) -> ActiveAppContext {
    let actx = crate::app_context::detect();
    let overrides = state.settings.lock().app_profile_overrides.clone();
    let profile = crate::app_context::category_for(&actx.exe, &actx.title, &overrides);
    ActiveAppContext {
        exe: actx.exe,
        title: actx.title,
        builtin_profile: actx.category,
        profile,
    }
}

#[derive(Serialize)]
pub struct AiTestResult {
    ok: bool,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    models: Option<Vec<crate::rewrite::ModelOption>>,
}

fn ai_test_plain(ok: bool, message: impl Into<String>) -> AiTestResult {
    AiTestResult {
        ok,
        message: message.into(),
        models: None,
    }
}

#[tauri::command]
pub fn ai_test(state: State<AppState>) -> AiTestResult {
    let (settings, backend, key, model, ollama_url, ollama_model) = {
        let s = state.settings.lock();
        (
            s.clone(),
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
                return ai_test_plain(false, "Введите API-ключ");
            }
            match crate::gemini::refine(
                &key,
                &model,
                "Ответь ровно одним словом.",
                "Напиши: ОК",
                &settings.proxy_url,
            ) {
                Ok(t) => ai_test_plain(true, format!("Gemini отвечает: {}", t.trim())),
                Err(e) => ai_test_plain(false, format!("Ошибка: {e}")),
            }
        }
        "ollama" => {
            // Сначала проверяем, что Ollama запущена и нужная модель скачана,
            // затем делаем «ОК»-пробу. Имена моделей бывают с тегом ("qwen3:4b"),
            // поэтому принимаем и точное совпадение, и префикс с двоеточием.
            match crate::ollama::list_models(&ollama_url) {
                Err(e) => ai_test_plain(false, format!("Ollama не запущена ({ollama_url}). {e}")),
                Ok(models)
                    if !models.iter().any(|m| {
                        m == &ollama_model || m.starts_with(&format!("{ollama_model}:"))
                    }) =>
                {
                    ai_test_plain(
                        false,
                        format!(
                            "Модель '{ollama_model}' не найдена. Скачайте: ollama pull {ollama_model}"
                        ),
                    )
                }
                Ok(_) => match crate::ollama::refine(
                    &ollama_url,
                    &ollama_model,
                    "Ответь ровно одним словом.",
                    "Напиши: ОК",
                ) {
                    Ok(t) => ai_test_plain(true, format!("Ollama отвечает: {}", t.trim())),
                    Err(e) => ai_test_plain(false, format!("Ошибка: {e}")),
                },
            }
        }
        "openai_compat" => {
            if crate::rewrite::is_openrouter_base(&settings.rewrite_base_url) {
                return match crate::rewrite::openrouter_free_models(&settings) {
                    Ok(models) if models.is_empty() => ai_test_plain(
                        false,
                        "OpenRouter ключ принят, но бесплатные текстовые модели не найдены",
                    ),
                    Ok(models) => AiTestResult {
                        ok: true,
                        message: format!(
                            "OpenRouter: найдено бесплатных моделей: {}",
                            models.len()
                        ),
                        models: Some(models),
                    },
                    Err(e) => ai_test_plain(false, format!("Ошибка: {e}")),
                };
            }
            if !crate::rewrite::configured(&settings) {
                return ai_test_plain(false, "Заполните Base URL и модель rewrite");
            }
            match crate::rewrite::ping(&settings) {
                Ok(t) => ai_test_plain(true, format!("Rewrite отвечает: {}", t.trim())),
                Err(e) => ai_test_plain(false, format!("Ошибка: {e}")),
            }
        }
        _ => ai_test_plain(false, "Движок ИИ выключен"),
    }
}

#[derive(Serialize)]
pub struct TransformResult {
    ok: bool,
    text: String,
    message: String,
}

const PROMPT_REWRITE_SYSTEM: &str = "Ты — редактор промптов для нейросетей. \
Перерабатывай исходный промпт строго по инструкции пользователя. \
Сохраняй исходный смысл, язык и важные детали. \
Не добавляй новые цели, ограничения или факты, которых нет в исходном промпте или инструкции. \
Верни только финальный переработанный промпт без пояснений.";

fn build_prompt_rewrite_request(
    original_prompt: &str,
    voice_instruction: &str,
) -> Result<(&'static str, String), String> {
    let original = original_prompt.trim();
    if original.is_empty() {
        return Err("Сначала напишите базовый prompt для переработки".into());
    }
    let instruction = voice_instruction.trim();
    if instruction.is_empty() {
        return Err("Сначала продиктуйте или введите инструкцию для переработки prompt".into());
    }
    let user = format!(
        "Исходный промпт пользователя:\n{original}\n\n\
         Голосовая инструкция пользователя:\n{instruction}\n\n\
         Задача:\n\
         Переработай исходный промпт строго с учетом голосовой инструкции.\n\
         Сохрани исходный смысл.\n\
         Сделай результат ясным, структурированным и готовым для отправки в нейросеть.\n\
         Не добавляй новые цели или ограничения, которых нет в исходном промпте или голосовой инструкции.\n\
         Верни только финальный переработанный промпт без пояснений."
    );
    Ok((PROMPT_REWRITE_SYSTEM, user))
}

#[tauri::command]
pub fn rewrite_prompt_with_instruction(
    state: State<AppState>,
    original_prompt: String,
    voice_instruction: String,
) -> TransformResult {
    let (system, user) = match build_prompt_rewrite_request(&original_prompt, &voice_instruction) {
        Ok(v) => v,
        Err(message) => {
            return TransformResult {
                ok: false,
                text: String::new(),
                message,
            };
        }
    };

    let s = state.settings.lock().clone();
    if s.ai_backend == "off" {
        return TransformResult {
            ok: false,
            text: String::new(),
            message: "Включите ИИ-бэкенд для голосовой переработки prompt".into(),
        };
    }

    let result = if s.ai_backend == "gemini" && crate::gemini::available(&s.ai_api_key) {
        crate::gemini::refine(&s.ai_api_key, &s.ai_model, system, &user, &s.proxy_url)
    } else if s.ai_backend == "openai_compat" && crate::rewrite::configured(&s) {
        crate::rewrite::refine(&s, system, &user)
    } else if s.ai_backend == "ollama" && crate::ollama::configured(&s.ollama_url) {
        crate::ollama::refine(&s.ollama_url, &s.ollama_model, system, &user)
    } else {
        Err(anyhow::anyhow!("выбранный ИИ-бэкенд не настроен"))
    };

    match result {
        Ok(out) if !out.trim().is_empty() => TransformResult {
            ok: true,
            text: out.trim().to_string(),
            message: "Готово".into(),
        },
        Ok(_) => TransformResult {
            ok: false,
            text: String::new(),
            message: "ИИ вернул пустой prompt".into(),
        },
        Err(e) => TransformResult {
            ok: false,
            text: String::new(),
            message: format!("Ошибка переработки prompt: {e}"),
        },
    }
}

#[tauri::command]
pub fn transform_text(state: State<AppState>, text: String, transform: String) -> TransformResult {
    let input = text.trim();
    if input.is_empty() {
        return TransformResult {
            ok: false,
            text: String::new(),
            message: "Введите текст для преобразования".into(),
        };
    }

    let transform_label = match transform.as_str() {
        "shorten" => "Сделай текст короче, сохрани смысл и язык.",
        "fix" => "Исправь ошибки, пунктуацию и капитализацию. Не добавляй новых фактов.",
        "prompt" => {
            "Преврати диктовку в ясный промпт для нейросети: действие, контекст, требования к результату и ограничения."
        }
        "formal" => "Перепиши текст деловым стилем, сохрани смысл и язык.",
        _ => "Аккуратно улучши текст, сохрани смысл и язык.",
    };

    let s = state.settings.lock().clone();
    if s.ai_backend == "off" {
        return TransformResult {
            ok: false,
            text: String::new(),
            message: "Включите ИИ-бэкенд для transforms".into(),
        };
    }

    let user =
        format!("[ПРИЛОЖЕНИЕ]: VoxFlow Scratchpad\n[ЗАДАЧА]: {transform_label}\n[ТЕКСТ]: {input}");

    let result = if s.ai_backend == "gemini" && crate::gemini::available(&s.ai_api_key) {
        crate::gemini::refine(
            &s.ai_api_key,
            &s.ai_model,
            "Верни только готовый преобразованный текст, без комментариев.",
            &user,
            &s.proxy_url,
        )
    } else if s.ai_backend == "openai_compat" && crate::rewrite::configured(&s) {
        crate::rewrite::refine(&s, crate::ollama::SYSTEM_PROMPT, &user)
    } else if s.ai_backend == "ollama" && crate::ollama::configured(&s.ollama_url) {
        crate::ollama::refine(
            &s.ollama_url,
            &s.ollama_model,
            crate::ollama::SYSTEM_PROMPT,
            &user,
        )
    } else {
        Err(anyhow::anyhow!("выбранный ИИ-бэкенд не настроен"))
    };

    match result {
        Ok(out) => TransformResult {
            ok: true,
            text: out.trim().to_string(),
            message: "Готово".into(),
        },
        Err(e) => TransformResult {
            ok: false,
            text: String::new(),
            message: format!("Ошибка transform: {e}"),
        },
    }
}

#[tauri::command]
pub fn default_app_profile_presets() -> Vec<ProfileOverride> {
    vec![
        ProfileOverride {
            pattern: "telegram".into(),
            profile: "casual".into(),
        },
        ProfileOverride {
            pattern: "whatsapp".into(),
            profile: "casual".into(),
        },
        ProfileOverride {
            pattern: "discord".into(),
            profile: "casual".into(),
        },
        ProfileOverride {
            pattern: "gmail".into(),
            profile: "formal".into(),
        },
        ProfileOverride {
            pattern: "outlook".into(),
            profile: "formal".into(),
        },
        ProfileOverride {
            pattern: "codex".into(),
            profile: "ai".into(),
        },
        ProfileOverride {
            pattern: "chatgpt".into(),
            profile: "ai".into(),
        },
        ProfileOverride {
            pattern: "claude".into(),
            profile: "ai".into(),
        },
        ProfileOverride {
            pattern: "gemini".into(),
            profile: "ai".into(),
        },
        ProfileOverride {
            pattern: "perplexity".into(),
            profile: "ai".into(),
        },
        ProfileOverride {
            pattern: "deepseek".into(),
            profile: "ai".into(),
        },
        ProfileOverride {
            pattern: "grok".into(),
            profile: "ai".into(),
        },
        ProfileOverride {
            pattern: "openrouter".into(),
            profile: "ai".into(),
        },
        ProfileOverride {
            pattern: "code.exe".into(),
            profile: "code".into(),
        },
        ProfileOverride {
            pattern: "cursor".into(),
            profile: "code".into(),
        },
        ProfileOverride {
            pattern: "windsurf".into(),
            profile: "code".into(),
        },
        ProfileOverride {
            pattern: "word".into(),
            profile: "doc".into(),
        },
        ProfileOverride {
            pattern: "google docs".into(),
            profile: "doc".into(),
        },
    ]
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
        return Ok(format!(
            "провайдер «{}» — не облачный (нечего проверять)",
            s.stt_provider
        ));
    }

    // Маленький тестовый WAV: 0.4 c тишины (16к * 0.4 = 6400 сэмплов).
    let wav = crate::paths::TempFileGuard::new(crate::paths::unique_tmp_path("stt_test", "wav"));
    if let Err(e) = crate::audio::write_wav_16k_mono(wav.path(), &vec![0.0f32; 6400]) {
        return Err(format!("не удалось создать тестовый WAV: {e}"));
    }

    let provider = s.stt_provider.clone();
    match crate::cloud_stt::transcribe(&s, wav.path()) {
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

// ─────────────────────────── Обновления ───────────────────────────

#[tauri::command]
pub fn check_for_update(state: State<AppState>) -> R<crate::updater::UpdateInfo> {
    let proxy = state.settings.lock().proxy_url.clone();
    crate::updater::check(&proxy).map_err(err)
}

#[tauri::command]
pub fn install_update(
    app: AppHandle,
    state: State<AppState>,
    asset_url: String,
    asset_name: String,
) -> R<crate::updater::UpdateInstallResult> {
    let proxy = state.settings.lock().proxy_url.clone();
    let result =
        crate::updater::download_and_launch(&asset_url, &asset_name, &proxy).map_err(err)?;

    state.engine.restore_auto_mute();
    let _ = state.engine_tx.lock().send(EngineCmd::Shutdown);

    // Даём IPC-ответу уйти во фронт и закрываемся, чтобы Inno мог заменить exe.
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(1200));
        app.exit(0);
    });

    Ok(result)
}

#[cfg(test)]
mod prompt_rewrite_tests {
    use super::build_prompt_rewrite_request;

    #[test]
    fn prompt_rewrite_request_requires_original_prompt() {
        let err = build_prompt_rewrite_request("   ", "сделай структурнее").unwrap_err();
        assert!(err.contains("базовый prompt"));
    }

    #[test]
    fn prompt_rewrite_request_requires_voice_instruction() {
        let err = build_prompt_rewrite_request("Напиши план запуска", "  ").unwrap_err();
        assert!(err.contains("инструкцию"));
    }

    #[test]
    fn prompt_rewrite_request_preserves_original_and_instruction() {
        let (system, user) = build_prompt_rewrite_request(
            "Structured Expert: помоги спланировать релиз",
            "добавь критерии приемки и сократи",
        )
        .unwrap();

        assert!(system.contains("редактор промптов"));
        assert!(user.contains("Structured Expert: помоги спланировать релиз"));
        assert!(user.contains("добавь критерии приемки и сократи"));
        assert!(user.contains("Не добавляй новые цели"));
        assert!(user.contains("Верни только финальный"));
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
