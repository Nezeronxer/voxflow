//! Настройки приложения. Хранятся одним JSON в kv['settings'].

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Settings {
    /// Клавиша hold-to-talk (rdev-имя), напр. "ControlRight".
    pub hotkey: String,
    /// Клавиша улучшения выделенного текста. Работает одиночным нажатием.
    pub improve_hotkey: String,
    /// "hold" | "toggle".
    pub mode: String,
    /// Имя устройства ввода ("" = системное по умолчанию).
    pub input_device: String,
    /// Язык ASR: "ru" | "en" | "auto" | ...
    pub language: String,
    /// Имя файла модели в models_dir.
    pub model: String,
    /// Локальный движок ASR: "gigaam" (русский, ONNX, CPU) | "whisper_cli" | "whisper_server".
    pub engine: String,
    /// Тема интерфейса: "system" | "light" | "dark".
    pub theme: String,
    /// Точная расшифровка без улучшений.
    pub verbatim: bool,
    /// Удалять слова-паразиты.
    pub remove_fillers: bool,
    /// Авто-пунктуация/капитализация (постобработка).
    pub auto_punct: bool,
    /// Тон: "very_casual" | "casual" | "neutral" | "formal".
    pub tone: String,
    /// Включить пользовательскую инструкцию стиля для модельного рерайта.
    pub smart_prompt_enabled: bool,
    /// Человеческое описание задачи/стиля, из которого UI собирает внутреннюю инструкцию.
    pub smart_prompt_source: String,
    /// Внутренняя инструкция для модели: как превращать диктовку в готовый текст.
    pub smart_prompt_instruction: String,
    /// "clipboard" | "type".
    pub paste_method: String,
    /// Звук старт/стоп.
    pub play_sounds: bool,
    /// Глушить системный вывод на время диктовки и восстанавливать после stop/Esc.
    pub auto_mute: bool,
    /// Автозапуск с системой.
    pub autostart: bool,
    /// Учиться на речи пользователя: сбор датасета (аудио↔текст) + адаптивный biasing.
    pub personalize: bool,
    /// ИИ-движок рефайна: "off" | "gemini" | "ollama" | "openai_compat".
    pub ai_backend: String,
    /// API-ключ ИИ (Google AI Studio / Gemini). Хранится локально, в URL не попадает.
    pub ai_api_key: String,
    /// Модель ИИ (напр. "gemini-2.5-flash").
    pub ai_model: String,
    /// URL локального Ollama (офлайн-рефайн через Qwen3).
    pub ollama_url: String,
    /// Модель Ollama (напр. "qwen3:4b").
    pub ollama_model: String,
    /// Использовать облачный ИИ для распознавания (ASR) вместо локального whisper.
    pub cloud_asr: bool,
    /// Legacy-флаг совместимости настроек; профили приложений теперь применяются всегда.
    pub tone_by_app: bool,
    /// Потоков для whisper (0 = авто).
    pub threads: u32,
    /// Режим живой вставки частичных результатов: "never" | "auto" | "always".
    /// Управляет ТОЛЬКО вставкой в поле во время речи; пилюля всё равно
    /// стримит живой текст, когда частичные результаты доступны (whisper_server).
    pub stream_mode: String,

    // --- Облачный STT (основной движок поверх локального распознавания) — D-022 ---
    /// Основной STT: "local" (whisper.cpp) | "openai_compat" (Avalon/OpenAI/Groq) | "deepgram".
    pub stt_provider: String,
    /// Авто-fallback на локальное распознавание при ошибке/недоступности облака.
    pub stt_fallback_local: bool,
    /// Живой ЧЕРНОВИК в плашке для ОБЛАЧНОГО STT: во время речи периодически слать
    /// растущий буфер в облако (Groq/Avalon) → серый текст в пилюле, «как у офлайн-
    /// моделей», но через API-ключ. Локальная модель/GPU при этом НЕ нужны.
    /// ЦЕНА: каждый тик заново транскрибирует растущий буфер (аудио-секунды копятся);
    /// движок ограничивает ≤CLOUD_DRAFT_CAP превью на диктовку (каденс ~2с, idle-стоп),
    /// но на бесплатных тирах (Groq) при активной диктовке квоту можно исчерпать —
    /// тогда упрётся и сам финал. По умолчанию включено (пользователь просил живой ввод);
    /// при экономии квоты — выключить тоггл (распознавание останется, без серого превью).
    pub cloud_live_draft: bool,
    /// OpenAI-совместимый STT — базовый URL
    /// (Avalon=https://api.aqua.sh/v1, OpenAI=https://api.openai.com/v1, Groq=https://api.groq.com/openai/v1).
    pub oai_stt_base_url: String,
    /// OpenAI-совместимый STT — модель (avalon-1 | gpt-4o-transcribe | whisper-large-v3 …).
    pub oai_stt_model: String,
    /// OpenAI-совместимый STT — API-ключ (заголовок Authorization: Bearer).
    /// Пусто → env STT_API_KEY / OPENAI_API_KEY / AVALON_API_KEY. НИКОГДА не логируется.
    pub oai_stt_key: String,
    /// Deepgram — базовый URL (https://api.deepgram.com | https://api.eu.deepgram.com).
    pub deepgram_base: String,
    /// Deepgram — модель (nova-3 …).
    pub deepgram_model: String,
    /// Deepgram — API-ключ (заголовок Authorization: Token). Пусто → env DEEPGRAM_API_KEY. Не логируется.
    pub deepgram_key: String,

    // --- Облачный rewrite (OpenAI-совместимый chat) ---
    /// Базовый URL OpenAI-совместимого chat-эндпойнта (без /chat/completions),
    /// напр. https://api.groq.com/openai/v1. Пусто → облачный rewrite выключен.
    pub rewrite_base_url: String,
    /// Chat-модель рефайна (llama-3.3-70b-versatile | gpt-4o-mini | claude-3-5-haiku …).
    pub rewrite_model: String,
    /// API-ключ rewrite (заголовок Authorization: Bearer).
    /// Пусто → env REWRITE_API_KEY / OPENAI_API_KEY. НИКОГДА не логируется.
    pub rewrite_key: String,

    /// Прокси для ВСЕХ внешних запросов (STT/LLM). Пусто → curl сам берёт
    /// HTTPS_PROXY/HTTP_PROXY из окружения. Формат: http://host:port.
    pub proxy_url: String,
    /// Пользовательские переопределения профиля тона по приложению
    /// (проверяются ПЕРЕД встроенной таблицей app_context).
    pub app_profile_overrides: Vec<ProfileOverride>,
    /// Пользовательские правила превращения диктовки в промпт для отдельных
    /// нейросетей/AI-чатов. Пустой список = только общий smart prompt.
    pub ai_prompt_rules: Vec<AiPromptRule>,
}

/// Пользовательское правило: если `pattern` встречается в имени exe или заголовке
/// окна (lowercase), применить профиль `profile`. Расширяет таблицу app_context
/// без перекомпиляции — «добавление приложения = строка» (бриф).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ProfileOverride {
    /// Подстрока для поиска в имени exe ИЛИ заголовке окна (сравнение в lowercase).
    #[serde(rename = "match")]
    pub pattern: String,
    /// Целевой профиль: verbatim | ai | formal | work | casual | doc | neutral.
    pub profile: String,
}

/// Правило для AI-окна: если `pattern` встречается в exe или заголовке, добавить
/// эти пользовательские инструкции к LLM-рерайту перед вставкой.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct AiPromptRule {
    /// Подстрока для поиска в имени exe ИЛИ заголовке окна.
    #[serde(rename = "match")]
    pub pattern: String,
    /// Пользовательские правила: как оформлять диктовку для этой нейросети.
    pub prompt: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hotkey: "ControlRight".into(),
            improve_hotkey: "F8".into(),
            mode: "hold".into(),
            input_device: String::new(),
            language: "auto".into(),
            model: "ggml-large-v3-turbo-q5_0.bin".into(),
            // Основной локальный движок — GigaAM-v3 e2e RNNT (русский SOTA, реальное
            // время на CPU, пунктуация из коробки). whisper остаётся для en/auto
            // и как fallback-модель в поле `model`.
            engine: "gigaam".into(),
            theme: "system".into(),
            verbatim: false,
            remove_fillers: true,
            auto_punct: true,
            tone: "neutral".into(),
            smart_prompt_enabled: true,
            smart_prompt_source: String::new(),
            smart_prompt_instruction: String::new(),
            paste_method: "clipboard".into(),
            play_sounds: true,
            auto_mute: true,
            autostart: false,
            personalize: true,
            ai_backend: "ollama".into(),
            ai_api_key: String::new(),
            ai_model: "gemini-2.5-flash".into(),
            ollama_url: "http://localhost:11434".into(),
            ollama_model: "qwen3:4b".into(),
            cloud_asr: false,
            tone_by_app: true,
            threads: 0,
            stream_mode: "never".into(),
            // Основной STT — локальный GigaAM (см. engine): ≤0.5с на CPU без сети
            // и без ключей. Облачные пресеты (Groq/Avalon/OpenAI/Deepgram) остаются
            // выбираемыми в UI (см. Stt.tsx) для en/спец-сценариев.
            stt_provider: "local".into(),
            stt_fallback_local: true,
            cloud_live_draft: true,
            oai_stt_base_url: "https://api.groq.com/openai/v1".into(),
            oai_stt_model: "whisper-large-v3".into(),
            oai_stt_key: String::new(),
            deepgram_base: "https://api.deepgram.com".into(),
            deepgram_model: "nova-3".into(),
            deepgram_key: String::new(),
            rewrite_base_url: String::new(),
            rewrite_model: String::new(),
            rewrite_key: String::new(),
            proxy_url: String::new(),
            app_profile_overrides: Vec::new(),
            ai_prompt_rules: Vec::new(),
        }
    }
}

impl Settings {
    pub fn redacted_for_renderer(mut self) -> Self {
        self.ai_api_key.clear();
        self.oai_stt_key.clear();
        self.deepgram_key.clear();
        self.rewrite_key.clear();
        self
    }

    pub fn preserve_empty_secrets_from(&mut self, existing: &Settings) {
        if self.ai_api_key.trim().is_empty() {
            self.ai_api_key = existing.ai_api_key.clone();
        }
        if self.oai_stt_key.trim().is_empty() {
            self.oai_stt_key = existing.oai_stt_key.clone();
        }
        if self.deepgram_key.trim().is_empty() {
            self.deepgram_key = existing.deepgram_key.clone();
        }
        if self.rewrite_key.trim().is_empty() {
            self.rewrite_key = existing.rewrite_key.clone();
        }
    }

    /// Число потоков для whisper (0 → половина логических ядер, минимум 2).
    pub fn effective_threads(&self) -> u32 {
        if self.threads > 0 {
            return self.threads;
        }
        let n = std::thread::available_parallelism()
            .map(|n| n.get() as u32)
            .unwrap_or(4);
        (n / 2).max(2)
    }

    /// Ключ для OpenAI-совместимого STT: из настроек, иначе из окружения.
    /// Пробует STT_API_KEY → OPENAI_API_KEY → AVALON_API_KEY. Не логируется.
    pub fn resolve_oai_key(&self) -> String {
        if !self.oai_stt_key.trim().is_empty() {
            return self.oai_stt_key.trim().to_string();
        }
        if !trusted_openai_compat_host(&self.oai_stt_base_url) {
            return String::new();
        }
        for k in ["STT_API_KEY", "OPENAI_API_KEY", "AVALON_API_KEY"] {
            if let Ok(v) = std::env::var(k) {
                if !v.trim().is_empty() {
                    return v.trim().to_string();
                }
            }
        }
        String::new()
    }

    /// Ключ для облачного rewrite (OpenAI-совместимый chat): из настроек, иначе
    /// из окружения. Пробует REWRITE_API_KEY → OPENROUTER_API_KEY → OPENAI_API_KEY.
    /// Не логируется.
    pub fn resolve_rewrite_key(&self) -> String {
        if !self.rewrite_key.trim().is_empty() {
            return self.rewrite_key.trim().to_string();
        }
        if !trusted_openai_compat_host(&self.rewrite_base_url) {
            return String::new();
        }
        for k in rewrite_key_env_order(&self.rewrite_base_url) {
            if let Ok(v) = std::env::var(k) {
                if !v.trim().is_empty() {
                    return v.trim().to_string();
                }
            }
        }
        String::new()
    }

    /// Облачный STT реально активен: провайдер облачный И ключ есть (из настроек
    /// или окружения). Без ключа облачный провайдер de-facto уходит в локальный
    /// фолбэк — поэтому все три потребителя (гард старта, петля партиалов, финал)
    /// сверяются именно с этим условием, а не только со строкой провайдера.
    pub fn cloud_stt_active(&self) -> bool {
        match self.stt_provider.as_str() {
            "openai_compat" => !self.resolve_oai_key().is_empty(),
            "deepgram" => !self.resolve_deepgram_key().is_empty(),
            _ => false,
        }
    }

    /// Ключ Deepgram: из настроек, иначе из env DEEPGRAM_API_KEY. Не логируется.
    pub fn resolve_deepgram_key(&self) -> String {
        if !self.deepgram_key.trim().is_empty() {
            return self.deepgram_key.trim().to_string();
        }
        if !trusted_deepgram_host(&self.deepgram_base) {
            return String::new();
        }
        std::env::var("DEEPGRAM_API_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_default()
    }
}

fn host_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let (_, rest) = trimmed.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host_port = authority.rsplit('@').next().unwrap_or("");
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split_once(']')?.0.to_string()
    } else {
        host_port.split(':').next().unwrap_or("").to_string()
    };
    let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn trusted_openai_compat_host(base_url: &str) -> bool {
    if crate::net::is_loopback_base_url(base_url) {
        return true;
    }
    matches!(
        host_from_url(base_url).as_deref(),
        Some("openrouter.ai" | "api.groq.com" | "api.openai.com" | "api.aqua.sh")
    )
}

fn trusted_deepgram_host(base_url: &str) -> bool {
    if crate::net::is_loopback_base_url(base_url) {
        return true;
    }
    matches!(
        host_from_url(base_url).as_deref(),
        Some("api.deepgram.com" | "api.eu.deepgram.com")
    )
}

fn rewrite_key_env_order(base_url: &str) -> [&'static str; 3] {
    if host_from_url(base_url).as_deref() == Some("openrouter.ai") {
        ["REWRITE_API_KEY", "OPENROUTER_API_KEY", "OPENAI_API_KEY"]
    } else {
        ["REWRITE_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"]
    }
}

pub fn load(conn: &Connection) -> Settings {
    match crate::db::kv_get(conn, "settings") {
        Some(j) => serde_json::from_str(&j).unwrap_or_default(),
        None => Settings::default(),
    }
}

pub fn save(conn: &Connection, s: &Settings) -> anyhow::Result<()> {
    let j = serde_json::to_string(s)?;
    crate::db::kv_set(conn, "settings", &j)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_key_env_order_prefers_openrouter_before_openai() {
        assert_eq!(
            rewrite_key_env_order("https://openrouter.ai/api/v1"),
            ["REWRITE_API_KEY", "OPENROUTER_API_KEY", "OPENAI_API_KEY"]
        );
        assert_eq!(
            rewrite_key_env_order("https://openrouter.ai.evil.test/api/v1"),
            ["REWRITE_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"]
        );
        assert_eq!(
            rewrite_key_env_order("https://api.openai.com/v1"),
            ["REWRITE_API_KEY", "OPENAI_API_KEY", "OPENROUTER_API_KEY"]
        );
    }

    #[test]
    fn legacy_settings_json_defaults_ai_prompt_rules() {
        let json = r#"{"hotkey":"ControlRight","ai_backend":"off"}"#;
        let s: Settings = serde_json::from_str(json).expect("legacy settings parse");
        assert_eq!(s.hotkey, "ControlRight");
        assert!(s.ai_prompt_rules.is_empty());
    }

    #[test]
    fn redacted_settings_do_not_expose_saved_keys() {
        let s = Settings {
            ai_api_key: "gemini-secret".into(),
            oai_stt_key: "stt-secret".into(),
            deepgram_key: "deepgram-secret".into(),
            rewrite_key: "rewrite-secret".into(),
            ..Settings::default()
        };
        let r = s.redacted_for_renderer();
        assert!(r.ai_api_key.is_empty());
        assert!(r.oai_stt_key.is_empty());
        assert!(r.deepgram_key.is_empty());
        assert!(r.rewrite_key.is_empty());
    }

    #[test]
    fn preserve_empty_secrets_keeps_existing_keys_but_accepts_replacements() {
        let existing = Settings {
            ai_api_key: "old-gemini".into(),
            oai_stt_key: "old-stt".into(),
            deepgram_key: "old-deepgram".into(),
            rewrite_key: "old-rewrite".into(),
            ..Settings::default()
        };
        let mut incoming = Settings {
            rewrite_key: "new-rewrite".into(),
            ..Settings::default()
        };
        incoming.preserve_empty_secrets_from(&existing);
        assert_eq!(incoming.ai_api_key, "old-gemini");
        assert_eq!(incoming.oai_stt_key, "old-stt");
        assert_eq!(incoming.deepgram_key, "old-deepgram");
        assert_eq!(incoming.rewrite_key, "new-rewrite");
    }

    #[test]
    fn env_fallback_only_for_known_or_loopback_hosts() {
        assert!(trusted_openai_compat_host("https://api.groq.com/openai/v1"));
        assert!(trusted_openai_compat_host("https://api.openai.com/v1"));
        assert!(trusted_openai_compat_host("http://localhost:11434/v1"));
        assert!(!trusted_openai_compat_host(
            "https://api.groq.com.evil.test/v1"
        ));
        assert!(!trusted_openai_compat_host("https://custom.example/v1"));

        assert!(trusted_deepgram_host("https://api.deepgram.com"));
        assert!(trusted_deepgram_host("https://api.eu.deepgram.com"));
        assert!(!trusted_deepgram_host("https://api.deepgram.com.evil.test"));
    }
}
