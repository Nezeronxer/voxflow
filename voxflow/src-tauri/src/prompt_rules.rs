//! Правила оформления промпта под КОНКРЕТНУЮ модель + подхват обновлений
//! вендорской документации.
//!
//! Зачем отдельный слой. Правила промптинга у Opus и Sonnet разные, у GPT и
//! Gemini — тем более, и вендоры их меняют. Держать это в Rust-таблице значило
//! бы, что каждое изменение документации требует новой сборки приложения.
//! Поэтому правила — ДАННЫЕ:
//!
//! 1. `prompt_rules.json` вшит в бинарь (`include_str!`) и всегда доступен —
//!    это базовый набор, составленный по документации на момент сборки;
//! 2. поверх него ложится кэш в `data_dir/prompt_rules_cache.json` — правила,
//!    пересобранные из свежей документации уже на машине пользователя.
//!
//! Как ловится обновление. У каждой модели есть URL её страницы документации.
//! Anthropic отдаёт их чистым markdown (`.md`), поэтому HTML парсить не нужно.
//! Фоновая проверка скачивает страницу, считает SHA-256 и сравнивает с хешем,
//! записанным при прошлой пересборке. Хеш не изменился — работы нет. Изменился —
//! документ уходит в уже настроенную пользователем LLM, которая выжимает из него
//! короткий список правил, и результат кладётся в кэш вместе с новым хешем.
//!
//! Границы честности: приложение подхватывает изменение САМОГО ДОКУМЕНТА. Оно не
//! умеет узнать о новой модели, страницы которой ещё нет в каталоге, — такая
//! приезжает с обновлением приложения.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::settings::Settings;

/// Базовый каталог, вшитый в бинарь. Используется, пока обновление не прошло
/// (нет сети, LLM не настроена, документ не менялся) — то есть всегда работает.
const BUNDLED: &str = include_str!("prompt_rules.json");

/// Как часто ходить за документацией. Страницы вендоров меняются раз в недели,
/// поэтому чаще суток проверять незачем.
const CHECK_INTERVAL_HOURS: i64 = 24;

/// Границы правдоподобия для пересобранных правил. Ответ короче нижней границы
/// означает, что модель не справилась; длиннее верхней — что она пересказала
/// документацию вместо выжимки. И то и другое хуже базовых правил, поэтому
/// такой результат не принимаем.
const DISTILLED_MIN_CHARS: usize = 80;
const DISTILLED_MAX_CHARS: usize = 900;

#[derive(Deserialize, Clone, Debug)]
pub struct PromptModel {
    /// Стабильный идентификатор, он же значение в настройках.
    pub id: String,
    /// Сервис из [`crate::app_context::ai_target`]: claude, chatgpt, gemini…
    pub service: String,
    /// Человеческое имя для выпадающего списка.
    pub label: String,
    /// Страница документации. Пустая строка — обновлять нечего.
    #[serde(default)]
    pub doc: String,
    /// Правила оформления промпта под эту модель.
    pub rules: String,
}

#[derive(Deserialize)]
struct Catalog {
    models: Vec<PromptModel>,
}

/// Одна пересобранная запись кэша.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct CachedRules {
    pub rules: String,
    /// SHA-256 документа, из которого правила выжаты. Совпал — работы нет.
    pub doc_hash: String,
    /// Когда последний раз ходили за документом (RFC3339). Нужен для throttle,
    /// поэтому пишется и когда документ не изменился.
    pub checked: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Cache {
    /// id модели → пересобранные правила.
    #[serde(default)]
    entries: std::collections::BTreeMap<String, CachedRules>,
}

fn cache_path() -> std::path::PathBuf {
    crate::paths::data_dir().join("prompt_rules_cache.json")
}

fn load_cache() -> Cache {
    std::fs::read_to_string(cache_path())
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &Cache) -> Result<()> {
    let path = cache_path();
    let raw = serde_json::to_string_pretty(cache).context("сериализация кэша правил")?;
    std::fs::write(&path, raw).with_context(|| format!("запись {path:?}"))?;
    // Кэш лежит рядом с БД и датасетом — те же права, что у остальных данных.
    let _ = crate::paths::set_private_file_permissions(&path);
    Ok(())
}

/// Каталог моделей из вшитого файла. Разбор не может провалиться в рантайме:
/// файл вшит и покрыт тестом, но панику всё равно не устраиваем.
pub fn catalog() -> Vec<PromptModel> {
    match serde_json::from_str::<Catalog>(BUNDLED) {
        Ok(c) => c.models,
        Err(e) => {
            log::error!("prompt_rules.json не разобран: {e}");
            Vec::new()
        }
    }
}

/// Какая модель выбрана для сервиса. Пользовательский выбор из настроек,
/// иначе первая модель сервиса из каталога (у Claude это Opus — старшая).
///
/// Сервис, которого в каталоге нет (Grok, DeepSeek, Copilot и прочие, что
/// `ai_target` узнаёт, но отдельной документации для них мы не ведём),
/// деградирует до записи `generic`: общая структура промпта лучше, чем ничего.
/// Выбор, указывающий на исчезнувшую модель, тоже откатывается к сервису —
/// иначе откат версии приложения молча выключал бы пересборку.
pub fn selected_model(s: &Settings, service: &str) -> Option<PromptModel> {
    let chosen = s
        .prompt_models
        .iter()
        .find(|c| c.service == service)
        .map(|c| c.model.trim())
        .filter(|v| !v.is_empty());
    let models = catalog();
    if let Some(id) = chosen {
        if let Some(found) = models.iter().find(|m| m.id == id) {
            return Some(found.clone());
        }
    }
    models
        .iter()
        .find(|m| m.service == service)
        .or_else(|| models.iter().find(|m| m.service == "generic"))
        .cloned()
}

/// Актуальные правила для модели: пересобранные из свежей документации, если
/// они есть, иначе базовые. Пустой кэш и отсутствие сети деградируют до базовых.
pub fn rules_for(model: &PromptModel) -> String {
    load_cache()
        .entries
        .get(&model.id)
        .map(|c| c.rules.trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| model.rules.clone())
}

/// Состояние правил модели для интерфейса: текст, источник и когда проверяли.
pub fn status_for(model: &PromptModel) -> (String, Option<CachedRules>) {
    let cached = load_cache().entries.get(&model.id).cloned();
    (rules_for(model), cached)
}

fn now_rfc3339() -> String {
    chrono::Local::now().to_rfc3339()
}

fn checked_recently(entry: Option<&CachedRules>) -> bool {
    let Some(entry) = entry else { return false };
    let Ok(when) = chrono::DateTime::parse_from_rfc3339(&entry.checked) else {
        return false;
    };
    let age = chrono::Local::now().signed_duration_since(when);
    age.num_hours() < CHECK_INTERVAL_HOURS
}

fn fetch_doc(url: &str, proxy: &str) -> Result<String> {
    let mut cmd = crate::net::curl();
    cmd.arg("-sSfL")
        .arg("-m")
        .arg("25")
        .arg("-A")
        .arg("VoxFlow");
    crate::net::apply_proxy(&mut cmd, proxy);
    cmd.arg(url);
    let out = cmd
        .output()
        .map_err(|e| anyhow!("не удалось запустить curl за документацией: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "документация недоступна ({}): {url}",
            out.status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "нет кода".into())
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if text.trim().len() < 500 {
        // Заглушка, редирект на логин или страница ошибки — из такого выжимать нечего.
        return Err(anyhow!("документация вернула подозрительно короткий ответ"));
    }
    Ok(text)
}

fn hash_of(text: &str) -> String {
    let mut h = Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// Инструкция для выжимки. Просим ИМЕННО то, что относится к формулировке
/// запроса человеком в чате: API-параметры (effort, thinking, max_tokens,
/// temperature) в промпт диктовки не превращаются и только заняли бы место.
fn distill_system_prompt() -> String {
    "Ты извлекаешь из документации правила формулирования запроса к языковой модели. \
     Отвечай ТОЛЬКО связным текстом правил на русском языке, без заголовков, списков с маркерами, \
     markdown и вступлений. Бери только то, что относится к тому, КАК ЧЕЛОВЕКУ СФОРМУЛИРОВАТЬ \
     запрос в чате: структура, что указывать явно, чего избегать в формулировке. \
     НЕ включай параметры API и настройки вызова (effort, thinking, max_tokens, temperature, \
     версии SDK, миграции, цены, лимиты) — их в текст запроса не пишут. \
     Уложись в 600 символов. Если в документе таких правил нет, ответь одним словом: НЕТ."
        .to_string()
}

fn distill(s: &Settings, model: &PromptModel, doc: &str) -> Result<String> {
    // Документация большая, а нужны только правила формулировки. Отдаём начало —
    // вендоры кладут поведенческие отличия в первые разделы, а хвост занимают
    // миграции и таблицы параметров.
    let slice: String = doc.chars().take(12000).collect();
    let user = format!(
        "Модель: {}.\nДокументация:\n\n{slice}\n\nВыдай правила формулирования запроса к этой модели.",
        model.label
    );
    let out = crate::engine::ask_configured_llm(s, &distill_system_prompt(), &user)
        .context("LLM не пересобрала правила")?;
    let out = out.trim();
    if out.is_empty() || out.eq_ignore_ascii_case("нет") {
        return Err(anyhow!("в документе не нашлось правил формулировки"));
    }
    let len = out.chars().count();
    if !(DISTILLED_MIN_CHARS..=DISTILLED_MAX_CHARS).contains(&len) {
        // Молча принять такой ответ означало бы ухудшить промпты пользователя
        // без единого следа в логе — поэтому это ошибка, а не «сойдёт».
        return Err(anyhow!(
            "выжимка неправдоподобной длины ({len} символов) — остаёмся на базовых правилах"
        ));
    }
    Ok(out.to_string())
}

/// Результат прохода обновления — для UI и лога.
#[derive(Serialize, Default, Debug)]
pub struct RefreshReport {
    pub checked: usize,
    pub updated: Vec<String>,
    pub skipped: Vec<String>,
    pub failed: Vec<String>,
}

/// Проверить документацию выбранных моделей и пересобрать правила изменившихся.
///
/// `force` игнорирует суточный throttle (кнопка «Обновить сейчас»). Без него
/// модели, проверенные меньше суток назад, пропускаются.
pub fn refresh(s: &Settings, models: &[PromptModel], force: bool) -> RefreshReport {
    let mut report = RefreshReport::default();
    // Выжимать правила нечем — качать документацию незачем. Иначе каждый запуск
    // ходил бы в сеть впустую и складывал все модели в failed.
    if !crate::engine::rewrite_backend_configured(s) {
        log::info!("правила промптов: бэкенд ИИ не настроен — проверку документации пропускаем");
        report.failed = models.iter().map(|m| m.id.clone()).collect();
        return report;
    }
    let mut cache = load_cache();
    let mut dirty = false;

    for model in models {
        if model.doc.trim().is_empty() {
            continue;
        }
        let entry = cache.entries.get(&model.id);
        if !force && checked_recently(entry) {
            report.skipped.push(model.id.clone());
            continue;
        }
        report.checked += 1;
        let doc = match fetch_doc(&model.doc, &s.proxy_url) {
            Ok(d) => d,
            Err(e) => {
                log::warn!("правила {}: документация не скачана: {e:#}", model.id);
                report.failed.push(model.id.clone());
                continue;
            }
        };
        let hash = hash_of(&doc);
        let known = entry.map(|e| e.doc_hash.as_str()).unwrap_or_default();
        if hash == known {
            // Документ не менялся — двигаем только отметку времени, чтобы не
            // ходить за ним повторно весь следующий день.
            if let Some(existing) = cache.entries.get_mut(&model.id) {
                existing.checked = now_rfc3339();
                dirty = true;
            }
            report.skipped.push(model.id.clone());
            continue;
        }
        match distill(s, model, &doc) {
            Ok(rules) => {
                cache.entries.insert(
                    model.id.clone(),
                    CachedRules {
                        rules,
                        doc_hash: hash,
                        checked: now_rfc3339(),
                    },
                );
                dirty = true;
                report.updated.push(model.id.clone());
                log::info!(
                    "правила {} пересобраны из обновлённой документации",
                    model.id
                );
            }
            Err(e) => {
                log::warn!("правила {}: выжимка не удалась: {e:#}", model.id);
                report.failed.push(model.id.clone());
            }
        }
    }

    if dirty {
        if let Err(e) = save_cache(&cache) {
            log::warn!("кэш правил не сохранён: {e:#}");
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Вшитый каталог обязан разбираться и покрывать все сервисы, которые умеет
    /// распознавать `app_context::ai_target`, — иначе для части нейросетей
    /// выпадающий список окажется пустым.
    #[test]
    fn bundled_catalog_parses_and_covers_detected_services() {
        let models = catalog();
        assert!(!models.is_empty(), "вшитый каталог не разобрался");
        for service in ["claude", "chatgpt", "codex", "gemini", "perplexity"] {
            assert!(
                models.iter().any(|m| m.service == service),
                "нет ни одной модели для сервиса {service}"
            );
        }
        for m in &models {
            assert!(!m.id.trim().is_empty(), "пустой id");
            assert!(!m.rules.trim().is_empty(), "у {} пустые правила", m.id);
        }
        let mut ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
        ids.sort_unstable();
        let unique = ids.len();
        ids.dedup();
        assert_eq!(unique, ids.len(), "повторяющиеся id моделей");
    }

    /// У Claude правила Opus и Sonnet обязаны различаться: ради этого раздел
    /// и разделён по номерам моделей, а не по семействам.
    #[test]
    fn claude_tiers_carry_different_rules() {
        let models = catalog();
        let opus = models
            .iter()
            .find(|m| m.id == "claude-opus-5")
            .expect("opus");
        let sonnet = models
            .iter()
            .find(|m| m.id == "claude-sonnet-5")
            .expect("sonnet");
        assert_ne!(opus.rules, sonnet.rules);
        assert_ne!(opus.doc, sonnet.doc);
    }

    #[test]
    fn selected_model_falls_back_to_first_of_service() {
        let mut s = Settings::default();
        // Незнакомый сервис не остаётся без правил — берётся общая запись.
        assert_eq!(
            selected_model(&s, "grok").expect("generic").service,
            "generic"
        );

        let default_claude = selected_model(&s, "claude").expect("модель по умолчанию");
        assert_eq!(default_claude.service, "claude");

        s.prompt_models = vec![crate::settings::PromptModelChoice {
            service: "claude".into(),
            model: "claude-sonnet-5".into(),
        }];
        assert_eq!(selected_model(&s, "claude").unwrap().id, "claude-sonnet-5");

        // Выбор указывает на модель, которой в каталоге больше нет (откат версии,
        // переименование) — не падаем и не отдаём пустоту, берём модель сервиса.
        s.prompt_models = vec![crate::settings::PromptModelChoice {
            service: "claude".into(),
            model: "claude-из-будущего".into(),
        }];
        assert_eq!(selected_model(&s, "claude").unwrap().service, "claude");
    }

    /// Хеш документа — единственный признак, по которому решается, тратить ли
    /// LLM на пересборку. Он обязан меняться от любой правки документа.
    #[test]
    fn doc_hash_changes_with_the_document() {
        let a = hash_of("Prompting Claude Opus 5\nGive the full task up front.");
        assert_eq!(
            a,
            hash_of("Prompting Claude Opus 5\nGive the full task up front.")
        );
        assert_ne!(
            a,
            hash_of("Prompting Claude Opus 5\nGive the full task up front!")
        );
    }

    /// Без настроенного бэкенда обновление обязано выйти СРАЗУ, не трогая сеть:
    /// выжимать скачанную документацию всё равно нечем. Заодно это делает тест
    /// герметичным — при рабочем бэкенде он полез бы наружу.
    #[test]
    fn refresh_without_a_backend_does_not_touch_the_network() {
        let s = Settings::default();
        assert_eq!(s.ai_backend, "off");

        let models = catalog();
        let report = refresh(&s, &models, true);

        assert_eq!(report.checked, 0, "сетевых проверок быть не должно");
        assert!(report.updated.is_empty());
        assert_eq!(report.failed.len(), models.len());

        // И правила при этом остаются рабочими — базовыми из сборки.
        let claude = selected_model(&s, "claude").expect("claude");
        assert_eq!(rules_for(&claude), claude.rules);
    }

    #[test]
    fn recent_check_is_skipped_but_missing_or_broken_timestamp_is_not() {
        let fresh = CachedRules {
            checked: now_rfc3339(),
            ..CachedRules::default()
        };
        assert!(checked_recently(Some(&fresh)));

        let stale = CachedRules {
            checked: (chrono::Local::now() - chrono::Duration::hours(CHECK_INTERVAL_HOURS + 1))
                .to_rfc3339(),
            ..CachedRules::default()
        };
        assert!(!checked_recently(Some(&stale)));

        assert!(!checked_recently(None));
        assert!(!checked_recently(Some(&CachedRules::default())));
    }
}
