//! Клиент OpenAI-совместимого облачного рефайна текста (/chat/completions).
//!
//! Третий бэкенд рерайта РЯДОМ с [`crate::gemini`] и [`crate::ollama`]
//! (выбор — `ai_backend == "openai_compat"`). Текстовая модель правит только
//! стиль/орфографию/пунктуацию по тому же системному промпту, что и Ollama.
//!
//! Цель — любые OpenAI-совместимые провайдеры через единый /chat/completions:
//! Groq (`https://api.groq.com/openai/v1`), OpenRouter
//! (`https://openrouter.ai/api/v1`), OpenAI, локальные/прокси-эндпоинты,
//! compat-обёртки над Claude/прочими. Различие только в `rewrite_base_url`,
//! `rewrite_model` и Bearer-ключе; для OpenRouter добавляем безопасный
//! attribution-заголовок `X-OpenRouter-Title`.
//!
//! Используется СИСТЕМНЫЙ `curl` (без reqwest — на машине нет cmake). Прокси —
//! через [`crate::net::curl_secret_with_proxy`] с `s.proxy_url`, чтобы облако
//! работало через настроенный прокси без утечки учетных данных в argv. Тело
//! пишем во временный файл и шлём `--data-binary @file`, как в
//! [`crate::gemini`]/[`crate::ollama`], чтобы не упираться в длину argv.
//!
//! ВАЖНО: ключ (`Authorization: Bearer …`) НИКОГДА не пишется в лог.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::net;

/// Нормализует базовый адрес: trim + срез хвостового `/`
/// (`https://api.groq.com/openai/v1/` → `https://api.groq.com/openai/v1`).
fn base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

fn host_from_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    let after_scheme = trimmed
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(trimmed);
    let host_port = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .rsplit('@')
        .next()
        .unwrap_or("")
        .trim();
    let host = host_port
        .split(':')
        .next()
        .unwrap_or("")
        .trim_matches(['[', ']'])
        .to_ascii_lowercase();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

pub(crate) fn is_openrouter_base(url: &str) -> bool {
    matches!(host_from_url(url).as_deref(), Some("openrouter.ai"))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ModelOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    name: Option<String>,
    pricing: Option<OpenRouterPricing>,
    #[serde(default)]
    output_modalities: Vec<String>,
    #[serde(default)]
    is_ready: Option<bool>,
    #[serde(default)]
    is_free: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterPricing {
    prompt: Option<String>,
    completion: Option<String>,
}

/// Настроен ли OpenAI-совместимый рефайн: непустые `rewrite_base_url` И
/// `rewrite_model` (после trim). Ключ тут НЕ проверяем — это делает [`refine`]
/// (пустой ключ → понятная ошибка, а не молчаливый пропуск конфигурации).
pub fn configured(s: &crate::settings::Settings) -> bool {
    !s.rewrite_base_url.trim().is_empty() && !s.rewrite_model.trim().is_empty()
}

/// Отрефайнить текст: `system` (инструкция) + `user` (исходный текст) через
/// POST {rewrite_base_url}/chat/completions (stream=false, temperature=0.2).
///
/// Зовётся из engine.rs как ветка `ai_backend == "openai_compat"` ровно тем же
/// набором аргументов, что и [`crate::ollama::refine`]: `system` =
/// [`crate::ollama::SYSTEM_PROMPT`], `user` = `build_voiceflow_payload(...)`.
///
/// Ключ берём из [`crate::settings::Settings::resolve_rewrite_key`] (настройки →
/// env REWRITE_API_KEY → OPENROUTER_API_KEY → OPENAI_API_KEY).
/// Пустой ключ → ошибка (без вызова сети).
pub fn refine(s: &crate::settings::Settings, system: &str, user: &str) -> Result<String> {
    let content = chat_completion(s, system, user, 0.2, Some(1200))?;
    let cleaned = cleanup_rewrite_output(&content, user);
    if cleaned.is_empty() {
        return Err(anyhow!(
            "OpenAI-совместимый рерайт вернул пустой ответ (нет текста в choices[0].message.content)"
        ));
    }
    if looks_like_reasoning(&cleaned, user) {
        log::warn!("OpenAI-compat: ответ похож на рассуждение/эхо промпта — деградация на правила");
        return Err(anyhow!(
            "OpenAI-совместимый рерайт вернул не финальный текст (рефайн пропущен)"
        ));
    }

    Ok(cleaned)
}

/// Лёгкая проверка chat-модели: короткий запрос, маленький лимит токенов, без
/// production cleanup. Используется кнопкой «Проверить» в UI.
pub fn ping(s: &crate::settings::Settings) -> Result<String> {
    let content = chat_completion(s, "Reply with exactly one word: OK.", "OK", 0.0, Some(8))?;
    let text = content.trim();
    if text.is_empty() {
        Err(anyhow!("OpenAI-совместимый рерайт вернул пустой ответ"))
    } else {
        Ok(text.to_string())
    }
}

/// Проверить OpenRouter-ключ и вернуть только бесплатные текстовые модели.
///
/// UI вызывает это через кнопку «Проверить»: до успешной проверки ключа список
/// моделей не показывается. Ключ уходит в curl через stdin-конфиг, не через argv.
pub fn openrouter_free_models(s: &crate::settings::Settings) -> Result<Vec<ModelOption>> {
    if !is_openrouter_base(&s.rewrite_base_url) {
        return Err(anyhow!("OpenRouter: выбран не OpenRouter Base URL"));
    }
    let key = s.resolve_rewrite_key();
    if key.trim().is_empty() {
        return Err(anyhow!(
            "OpenRouter: не задан API-ключ (rewrite_key / env REWRITE_API_KEY / OPENROUTER_API_KEY)"
        ));
    }

    // Сначала валидируем именно ключ. /models тоже требует Bearer, но отдельный
    // /key даёт понятный отказ для fake/пустых ключей и не зависит от модели.
    let api_base = base(&s.rewrite_base_url);
    let _ = openrouter_get_json(s, &format!("{api_base}/key"), &key, 10)?;
    let v = openrouter_get_json(
        s,
        &format!("{api_base}/models?output_modalities=text&sort=pricing-low-to-high"),
        &key,
        15,
    )?;
    let response: OpenRouterModelsResponse = serde_json::from_value(v)
        .map_err(|e| anyhow!("OpenRouter models: неожиданный JSON: {e}"))?;
    let mut out = free_model_options(response.data);
    out.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    Ok(out)
}

fn openrouter_get_json(
    s: &crate::settings::Settings,
    endpoint: &str,
    key: &str,
    timeout_s: u64,
) -> Result<serde_json::Value> {
    net::ensure_https_or_loopback_base(endpoint, "OpenRouter endpoint")?;
    let mut cmd = net::curl();
    cmd.arg("-s")
        .arg("-m")
        .arg(timeout_s.to_string())
        .arg("-H")
        .arg("Accept: application/json")
        .arg("-X")
        .arg("GET")
        .arg(endpoint);

    let secret_headers = vec![
        format!("Authorization: Bearer {key}"),
        "X-OpenRouter-Title: VoxFlow".to_string(),
    ];
    let out = net::curl_secret_with_proxy(cmd, &secret_headers, &s.proxy_url)
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("curl завершился с ошибкой: {}", err.trim()));
    }

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow!("OpenRouter ответил не JSON: {e}"))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .or_else(|| err.as_str())
            .unwrap_or("неизвестная ошибка OpenRouter");
        return Err(anyhow!("OpenRouter API error: {msg}"));
    }
    Ok(v)
}

fn free_model_options(models: Vec<OpenRouterModel>) -> Vec<ModelOption> {
    models
        .into_iter()
        .filter(is_free_text_model)
        .map(|m| {
            let name = m.name.unwrap_or_else(|| m.id.clone());
            let label = if name == m.id {
                name
            } else {
                format!("{name} · {}", m.id)
            };
            ModelOption { value: m.id, label }
        })
        .collect()
}

fn is_free_text_model(model: &OpenRouterModel) -> bool {
    if model.is_ready == Some(false) {
        return false;
    }
    if !model.output_modalities.is_empty()
        && !model
            .output_modalities
            .iter()
            .any(|m| m.eq_ignore_ascii_case("text"))
    {
        return false;
    }
    model.is_free == Some(true)
        || model.id.to_ascii_lowercase().ends_with(":free")
        || model
            .pricing
            .as_ref()
            .map(pricing_prompt_completion_zero)
            .unwrap_or(false)
}

fn pricing_prompt_completion_zero(pricing: &OpenRouterPricing) -> bool {
    match (&pricing.prompt, &pricing.completion) {
        (Some(prompt), Some(completion)) => is_zero_price(prompt) && is_zero_price(completion),
        _ => false,
    }
}

fn is_zero_price(raw: &str) -> bool {
    raw.trim().parse::<f64>().map(|v| v == 0.0).unwrap_or(false)
}

fn chat_completion(
    s: &crate::settings::Settings,
    system: &str,
    user: &str,
    temperature: f64,
    max_tokens: Option<u32>,
) -> Result<String> {
    // Ключ резолвим первым: пустой → не ходим в сеть, отдаём понятную ошибку.
    let key = s.resolve_rewrite_key();
    if key.trim().is_empty() {
        return Err(anyhow!(
            "OpenAI-совместимый рерайт: не задан ключ (rewrite_key / env REWRITE_API_KEY / OPENROUTER_API_KEY / OPENAI_API_KEY)"
        ));
    }

    let base_url = base(&s.rewrite_base_url);
    net::ensure_https_or_loopback_base(&base_url, "Rewrite Base URL")?;
    let endpoint = format!("{base_url}/chat/completions");

    let mut body = serde_json::json!({
        "model": s.rewrite_model.trim(),
        "messages": [
            { "role": "system", "content": system },
            { "role": "user",   "content": user }
        ],
        "temperature": temperature,
        "stream": false
    });
    if let Some(max_tokens) = max_tokens {
        body["max_tokens"] = serde_json::json!(max_tokens);
    }

    // Тело запроса — во временный файл (как в gemini.rs/ollama.rs): не упираемся в argv.
    let payload = serde_json::to_vec(&body).map_err(|e| anyhow!("сериализация тела: {e}"))?;
    let req = net::TempPayload::write_json("rewrite_req", &payload)?;
    let data_arg = req.curl_data_arg();
    let mut secret_headers = vec![format!("Authorization: Bearer {key}")];
    if is_openrouter_base(&base_url) {
        secret_headers.push("X-OpenRouter-Title: VoxFlow".to_string());
    }

    // Прокси-aware curl из общего модуля net (CREATE_NO_WINDOW уже внутри).
    // Явный proxy_url уходит через stdin-config, чтобы user:pass@proxy не был виден в argv.
    let mut cmd = net::curl();
    cmd.arg("-s")
        .arg("-m")
        // Рефайн — СИНХРОННЫЙ шаг перед вставкой текста: дольше ~10с он
        // обесценивает диктовку (пользователь уже ждёт). Не успел — вставляем
        // текст после правил (graceful-деградация выше по стеку).
        .arg("10")
        // Content-Type не секрет — остаётся в argv.
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-X")
        .arg("POST")
        .arg("--data-binary")
        .arg(&data_arg)
        .arg(&endpoint);

    // Ключ (Bearer) — через stdin-конфиг curl (-K -), НЕ в argv:
    // командная строка процесса видна другим процессам пользователя.
    let out = net::curl_secret_with_proxy(cmd, &secret_headers, &s.proxy_url)
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

    if !out.status.success() && out.stdout.is_empty() {
        // curl упал без тела (сеть/таймаут/нет прокси) — stderr безопасен (без ключа).
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("curl завершился с ошибкой: {}", err.trim()));
    }

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("ответ рерайта — не JSON: {e}"))?;

    // Явная ошибка API. Совместимые провайдеры отдают либо объект
    // {"error":{"message":"…"}}, либо строкой {"error":"…"} — обрабатываем оба.
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .or_else(|| err.as_str())
            .unwrap_or("неизвестная ошибка рерайта");
        return Err(anyhow!("Rewrite API error: {msg}"));
    }

    // Ответ chat-эндпоинта: choices[0].message.content.
    let content = v
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("");

    if content.trim().is_empty() {
        // Пустой ответ без поля error — отдаём диагностику без ключа.
        log::warn!("Rewrite вернул пустой текст; raw len={}", out.stdout.len());
        return Err(anyhow!(
            "OpenAI-совместимый рерайт вернул пустой ответ (нет текста в choices[0].message.content)"
        ));
    }
    Ok(content.to_string())
}

fn cleanup_rewrite_output(text: &str, input: &str) -> String {
    let mut s = strip_think(text);
    for prefix in [
        "Готовый текст:",
        "Вот готовый текст:",
        "Финальный текст:",
        "Результат:",
        "Output:",
        "Final:",
    ] {
        if s.to_lowercase().starts_with(&prefix.to_lowercase()) {
            s = s[prefix.len()..].trim_start().to_string();
        }
    }
    let trimmed_input = input.trim();
    if s.trim() == trimmed_input {
        return String::new();
    }
    s.trim_matches('"').trim().to_string()
}

fn strip_think(text: &str) -> String {
    let mut s = text.to_string();
    while let Some(start) = s.find("<think>") {
        match s[start..].find("</think>") {
            Some(end_rel) => {
                let end = start + end_rel + "</think>".len();
                s.replace_range(start..end, "");
            }
            None => {
                s.truncate(start);
                break;
            }
        }
    }
    if let Some(end_rel) = s.find("</think>") {
        s.replace_range(0..end_rel + "</think>".len(), "");
    }
    s.replace("/no_think", "").trim().to_string()
}

fn looks_like_reasoning(out: &str, input: &str) -> bool {
    let low = out.to_lowercase();
    if ["[приложение]", "[диктовка]", "[окружение]", "/no_think"]
        .iter()
        .any(|m| low.contains(m))
    {
        return true;
    }
    let out_len = out.chars().count();
    let in_len = input.chars().count().max(1);
    let process_markers = [
        "перепишу",
        "исходный текст",
        "надиктованн",
        "the user",
        "i need to",
        "rewrite the",
    ];
    out_len > in_len * 2 && process_markers.iter().any(|m| low.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_base_detection_is_lenient() {
        assert!(is_openrouter_base("https://openrouter.ai/api/v1"));
        assert!(is_openrouter_base(" https://OPENROUTER.AI/api/v1/ "));
        assert!(is_openrouter_base("https://token@openrouter.ai:443/api/v1"));
        assert!(!is_openrouter_base("https://not-openrouter.ai/api/v1"));
        assert!(!is_openrouter_base(
            "https://openrouter.ai.evil.test/api/v1"
        ));
        assert!(!is_openrouter_base("https://api.openai.com/v1"));
    }

    #[test]
    fn openrouter_free_filter_keeps_only_free_text_models() {
        let models = vec![
            OpenRouterModel {
                id: "deepseek/deepseek-r1:free".into(),
                name: Some("DeepSeek R1 Free".into()),
                pricing: Some(OpenRouterPricing {
                    prompt: Some("0".into()),
                    completion: Some("0.000000".into()),
                }),
                output_modalities: vec!["text".into()],
                is_ready: Some(true),
                is_free: Some(true),
            },
            OpenRouterModel {
                id: "paid/model".into(),
                name: Some("Paid".into()),
                pricing: Some(OpenRouterPricing {
                    prompt: Some("0.000001".into()),
                    completion: Some("0".into()),
                }),
                output_modalities: vec!["text".into()],
                is_ready: Some(true),
                is_free: Some(false),
            },
            OpenRouterModel {
                id: "image/free:free".into(),
                name: Some("Image".into()),
                pricing: Some(OpenRouterPricing {
                    prompt: Some("0".into()),
                    completion: Some("0".into()),
                }),
                output_modalities: vec!["image".into()],
                is_ready: Some(true),
                is_free: Some(true),
            },
            OpenRouterModel {
                id: "not-ready:free".into(),
                name: Some("Not Ready".into()),
                pricing: Some(OpenRouterPricing {
                    prompt: Some("0".into()),
                    completion: Some("0".into()),
                }),
                output_modalities: vec!["text".into()],
                is_ready: Some(false),
                is_free: Some(true),
            },
        ];

        let out = free_model_options(models);
        assert_eq!(
            out,
            vec![ModelOption {
                value: "deepseek/deepseek-r1:free".into(),
                label: "DeepSeek R1 Free · deepseek/deepseek-r1:free".into(),
            }]
        );
    }
}
