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
//! `rewrite_model` и ключе; для OpenRouter добавляем безопасный
//! attribution-заголовок `X-OpenRouter-Title`. Сервисам вне OpenAI-конвенции
//! имя заголовка с ключом задаётся в `rewrite_auth_header` (`x-api-key` и др.).
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

/// Модель постобработки, которую советуем в OpenRouter (D-058): самая дешёвая из
/// тех, что на диктовках не теряют слов, не вырезают мат и чинят ослышки.
pub const OPENROUTER_RECOMMENDED_MODEL: &str = "anthropic/claude-haiku-5.5";

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

/// Строка заголовка с ключом. По умолчанию — OpenAI-конвенция
/// `Authorization: Bearer {key}`; своё имя заголовка (`x-api-key`, `api-key`)
/// шлёт ключ как есть — так подключается провайдер вне конвенции.
/// Имя уже отфильтровано `Settings::normalize_user_values`.
fn auth_header_line(header_name: &str, key: &str) -> String {
    let name = header_name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("authorization") {
        format!("Authorization: Bearer {key}")
    } else {
        format!("{name}: {key}")
    }
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
/// Зовётся из engine.rs как ветка `ai_backend == "openai_compat"` тем же
/// промптом стенографиста, что и локальные модели (D-058): один для всех
/// программ. «Улучшить выделенное» шлёт сюда `SYSTEM_PROMPT` со стилем «doc».
///
/// Ключ берём из [`crate::settings::Settings::resolve_rewrite_key`] (настройки →
/// env REWRITE_API_KEY → OPENROUTER_API_KEY → OPENAI_API_KEY).
/// Пустой ключ → ошибка (без вызова сети).
pub fn refine(s: &crate::settings::Settings, system: &str, user: &str) -> Result<String> {
    // Лимит вывода — от длины входа, а не жёсткие 1200: для русского это ~2500
    // символов, и длинная диктовка обрывалась на полуслове.
    let max_tokens = net::output_token_budget(
        net::estimate_tokens(system) + net::estimate_tokens(user),
        s.rewrite_max_output_tokens,
    );
    let content = chat_completion(s, system, user, 0.2, Some(max_tokens))?;
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
    // 32, а не 8: модели с обязательным рассуждением тратят пару токенов и на него.
    let content = chat_completion(s, "Reply with exactly one word: OK.", "OK", 0.0, Some(32))?;
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
/// Каталог текстовых моделей OpenRouter: рекомендованная первой, дальше платные
/// от дешёвых к дорогим, бесплатные (с лимитами, часто недоступны) - в конце.
/// Цена в подписи: для постобработки хватает самых дешёвых (D-058).
pub fn openrouter_models(s: &crate::settings::Settings) -> Result<Vec<ModelOption>> {
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
    Ok(text_model_options(response.data))
}

/// Каталог моделей провайдера: `GET {base}/models` по ключу. Ответ ждём в
/// формате OpenAI (`data[].id`); Gemini в OpenAI-режиме и большинство
/// совместимых сервисов отдают его же. Список отсортирован по имени.
pub fn list_models(s: &crate::settings::Settings) -> Result<Vec<ModelOption>> {
    let base_url = s.rewrite_base_url.trim();
    if base_url.is_empty() {
        return Err(anyhow!("не задан Base URL"));
    }
    let key = s.resolve_rewrite_key();
    let api_base = base(base_url);
    let v = get_json_authed(
        s,
        &format!("{api_base}/models"),
        &key,
        15,
        "каталог моделей",
    )?;
    let ids = model_ids_from_catalog(&v);
    if ids.is_empty() {
        return Err(anyhow!("сервис вернул пустой список моделей"));
    }
    Ok(ids
        .into_iter()
        .map(|id| ModelOption {
            label: id.clone(),
            value: id,
        })
        .collect())
}

/// Идентификаторы моделей из `/models`: OpenAI-формат `data[].id`, у части
/// сервисов — `models[].id` или `models[].name`. Дубликаты убраны, порядок —
/// по имени, префикс `models/` (Gemini) срезан.
fn model_ids_from_catalog(v: &serde_json::Value) -> Vec<String> {
    let list = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();
    let mut ids: Vec<String> = list
        .iter()
        .filter_map(|m| {
            m.get("id")
                .or_else(|| m.get("name"))
                .and_then(|id| id.as_str())
                .map(|id| id.trim_start_matches("models/").to_string())
        })
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort_by_key(|a| a.to_lowercase());
    ids.dedup();
    ids
}

fn openrouter_get_json(
    s: &crate::settings::Settings,
    endpoint: &str,
    key: &str,
    timeout_s: u64,
) -> Result<serde_json::Value> {
    get_json_authed(s, endpoint, key, timeout_s, "OpenRouter")
}

/// GET JSON с ключом в заголовке (через stdin-конфиг curl, не argv). Пустой
/// ключ — запрос без авторизации: локальные серверы вроде LM Studio его не ждут.
fn get_json_authed(
    s: &crate::settings::Settings,
    endpoint: &str,
    key: &str,
    timeout_s: u64,
    label: &str,
) -> Result<serde_json::Value> {
    net::ensure_https_or_loopback_base(endpoint, &format!("{label} endpoint"))?;
    let mut cmd = net::curl();
    cmd.arg("-s")
        .arg("-m")
        .arg(timeout_s.to_string())
        .arg("-H")
        .arg("Accept: application/json")
        .arg("-X")
        .arg("GET")
        .arg(endpoint);

    let mut secret_headers = vec!["X-OpenRouter-Title: VoxFlow".to_string()];
    if !key.trim().is_empty() {
        secret_headers.push(auth_header_line(&s.rewrite_auth_header, key));
    }
    let out = net::curl_secret_with_proxy(cmd, &secret_headers, &s.proxy_url)
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;
    if !out.status.success() && out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("curl завершился с ошибкой: {}", err.trim()));
    }

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("{label}: ответ не JSON: {e}"))?;
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .or_else(|| err.as_str())
            .unwrap_or("неизвестная ошибка");
        return Err(anyhow!("{label}: {msg}"));
    }
    Ok(v)
}

fn text_model_options(models: Vec<OpenRouterModel>) -> Vec<ModelOption> {
    // (рекомендована, бесплатна, цена за 1M вход+выход, id) - ключ сортировки.
    let mut rows: Vec<(bool, bool, f64, ModelOption)> = models
        .into_iter()
        .filter(is_text_model)
        .filter_map(|m| {
            let free = is_free_model(&m);
            let (prompt, completion) = per_million(m.pricing.as_ref());
            // Цена неизвестна или отрицательна (роутеры вроде openrouter/auto) - не
            // показываем: человек выбирает по цене, а её нет.
            if !free && (prompt? < 0.0 || completion? < 0.0) {
                return None;
            }
            let (prompt, completion) = (prompt.unwrap_or(0.0), completion.unwrap_or(0.0));
            let recommended = m.id == OPENROUTER_RECOMMENDED_MODEL;
            let name = m.name.unwrap_or_else(|| m.id.clone());
            let price = if free {
                "бесплатно, с лимитами".to_string()
            } else {
                format!("${prompt:.2} / ${completion:.2} за 1M")
            };
            let label = format!(
                "{name} · {price}{}",
                if recommended {
                    " · рекомендуем"
                } else {
                    ""
                }
            );
            Some((
                recommended,
                free,
                prompt + completion,
                ModelOption { value: m.id, label },
            ))
        })
        .collect();
    rows.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.total_cmp(&b.2))
            .then_with(|| a.3.value.cmp(&b.3.value))
    });
    rows.into_iter().map(|row| row.3).collect()
}

/// Готовая модель, отвечает текстом. `:batch` - пакетные эндпоинты, на
/// синхронный запрос диктовки не отвечают.
fn is_text_model(model: &OpenRouterModel) -> bool {
    if model.is_ready == Some(false) || model.id.ends_with(":batch") {
        return false;
    }
    model.output_modalities.is_empty()
        || model
            .output_modalities
            .iter()
            .any(|m| m.eq_ignore_ascii_case("text"))
}

fn is_free_model(model: &OpenRouterModel) -> bool {
    model.is_free == Some(true)
        || model.id.to_ascii_lowercase().ends_with(":free")
        || model
            .pricing
            .as_ref()
            .map(pricing_prompt_completion_zero)
            .unwrap_or(false)
}

/// Цена за 1M токенов (вход, выход); в API она за один токен строкой.
fn per_million(pricing: Option<&OpenRouterPricing>) -> (Option<f64>, Option<f64>) {
    let parse = |v: &Option<String>| v.as_deref()?.trim().parse::<f64>().ok().map(|p| p * 1e6);
    pricing.map_or((None, None), |p| (parse(&p.prompt), parse(&p.completion)))
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
    // Рассуждения правке не нужны: с ними ответ медленнее, а Haiku при
    // `effort: minimal` пишет их прямо в текст (D-058). Модели с обязательным
    // рассуждением отказывают на `enabled: false` - им повтор с самым коротким.
    let openrouter = is_openrouter_base(&base_url);
    if openrouter {
        body["reasoning"] = serde_json::json!({ "enabled": false });
    }
    let first = match take_warm(s, &endpoint, &key) {
        // Соединение могло закрыться на стороне сервера - тогда обычный запрос.
        Some(warm) => warm.send(&body).or_else(|e| {
            log::warn!("прогретое соединение не сработало ({e}), обычный запрос");
            post_chat(s, &base_url, &endpoint, &key, &body)
        }),
        None => post_chat(s, &base_url, &endpoint, &key, &body),
    };
    match first {
        Err(e) if openrouter && reasoning_is_mandatory(&e) => {
            body["reasoning"] = serde_json::json!({ "effort": "minimal" });
            post_chat(s, &base_url, &endpoint, &key, &body)
        }
        result => result,
    }
}

/// Сколько прогретое соединение ждёт текст. Сервер OpenRouter держит открытый
/// запрос без тела не меньше минуты (60 с - работает, 150 с - закрыт).
const WARM_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(90);

/// Запрос к OpenRouter, открытый при нажатии клавиши: curl уже прошёл TLS и
/// ждёт тело на stdin (`-T -`, chunked). Через прокси этого мака рукопожатие
/// стоит 1,5-3,8 с - на диктовке его больше не ждём (D-059).
struct Warm {
    // None - процесс уже отдан в send() и ждётся там.
    child: Option<std::process::Child>,
    endpoint: String,
    key: String,
    proxy: String,
    started: std::time::Instant,
    timeout_s: u64,
    // Конфиг curl с ключом: приватный файл, удаляется вместе с Warm.
    _config: crate::paths::TempFileGuard,
}

impl Warm {
    fn send(mut self, body: &serde_json::Value) -> Result<String> {
        use std::io::Write;
        let payload = serde_json::to_vec(body).map_err(|e| anyhow!("сериализация тела: {e}"))?;
        let mut child = self.child.take().ok_or_else(|| anyhow!("curl уже отдан"))?;
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow!("stdin уже закрыт"))?;
            stdin.write_all(&payload)?;
        } // EOF - конец тела
        parse_curl_chat(child.wait_with_output()?, self.timeout_s)
    }
}

impl Drop for Warm {
    fn drop(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

static WARM: std::sync::Mutex<Option<Warm>> = std::sync::Mutex::new(None);

/// Открыть соединение к OpenRouter заранее (зовётся на старте диктовки).
/// Прошлое неиспользованное закрывается. Ошибки молча: тогда обычный запрос.
pub fn prewarm(s: &crate::settings::Settings) {
    let base_url = base(&s.rewrite_base_url);
    let key = s.resolve_rewrite_key();
    if s.ai_backend != "openai_compat"
        || !configured(s)
        || !is_openrouter_base(&base_url)
        || key.trim().is_empty()
    {
        return;
    }
    let endpoint = format!("{base_url}/chat/completions");
    let timeout_s = s.backend_timeout_s(false);
    let config_path = crate::paths::unique_tmp_path("curl_cfg", "txt");
    let guard = crate::paths::TempFileGuard::new(config_path.clone());
    let mut lines = vec![
        net::secret_header_line(&auth_header_line(&s.rewrite_auth_header, &key)),
        net::secret_header_line("X-OpenRouter-Title: VoxFlow"),
    ];
    lines.extend(net::proxy_config_line(&s.proxy_url));
    let written = crate::paths::create_private_file(&config_path).and_then(|mut f| {
        use std::io::Write;
        f.write_all((lines.join("\n") + "\n").as_bytes())
    });
    if written.is_err() {
        return;
    }
    let mut cmd = net::curl();
    cmd.arg("-s")
        .arg("-m")
        .arg((WARM_MAX_AGE.as_secs() + timeout_s).to_string())
        .arg("-K")
        .arg(&config_path)
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-X")
        .arg("POST")
        .arg("-T")
        .arg("-")
        .arg(&endpoint)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let Ok(child) = cmd.spawn() else {
        return;
    };
    let warm = Warm {
        child: Some(child),
        endpoint,
        key,
        proxy: s.proxy_url.clone(),
        started: std::time::Instant::now(),
        timeout_s,
        _config: guard,
    };
    *WARM.lock().unwrap_or_else(|e| e.into_inner()) = Some(warm);
}

/// Прогретое соединение для этого запроса, если оно свежее и к тому же адресу,
/// с тем же ключом и прокси. Иначе закрываем его и идём обычным путём.
fn take_warm(s: &crate::settings::Settings, endpoint: &str, key: &str) -> Option<Warm> {
    let mut warm = WARM.lock().unwrap_or_else(|e| e.into_inner()).take()?;
    let alive = matches!(warm.child.as_mut().map(|c| c.try_wait()), Some(Ok(None)));
    (alive
        && warm.started.elapsed() < WARM_MAX_AGE
        && warm.endpoint == endpoint
        && warm.key == key
        && warm.proxy == s.proxy_url)
        .then_some(warm)
}

fn reasoning_is_mandatory(e: &anyhow::Error) -> bool {
    e.to_string()
        .to_lowercase()
        .contains("reasoning is mandatory")
}

/// Один POST /chat/completions: тело во временный файл, ключ через stdin curl.
fn post_chat(
    s: &crate::settings::Settings,
    base_url: &str,
    endpoint: &str,
    key: &str,
    body: &serde_json::Value,
) -> Result<String> {
    // Тело запроса — во временный файл (как в gemini.rs/ollama.rs): не упираемся в argv.
    let payload = serde_json::to_vec(body).map_err(|e| anyhow!("сериализация тела: {e}"))?;
    let req = net::TempPayload::write_json("rewrite_req", &payload)?;
    let data_arg = req.curl_data_arg();
    let mut secret_headers = vec![auth_header_line(&s.rewrite_auth_header, key)];
    if is_openrouter_base(base_url) {
        secret_headers.push("X-OpenRouter-Title: VoxFlow".to_string());
    }

    // Прокси-aware curl из общего модуля net (CREATE_NO_WINDOW уже внутри).
    // Облако из РФ: прокси берём из настроек (s.proxy_url); пустой → net::apply_proxy
    // не добавляет -x, и curl сам читает HTTPS_PROXY/HTTP_PROXY из окружения.
    let timeout_s = s.backend_timeout_s(net::is_loopback_base_url(base_url));
    let mut cmd = net::curl();
    cmd.arg("-s")
        .arg("-m")
        // Таймаут — из настроек (`ai_timeout_s`), локальный эндпоинт получает
        // увеличенный. Не успел — вставляем текст после правил.
        .arg(timeout_s.to_string())
        // Content-Type не секрет — остаётся в argv.
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-X")
        .arg("POST")
        .arg("--data-binary")
        .arg(&data_arg)
        .arg(endpoint);

    // Ключ (Bearer) — через stdin-конфиг curl (-K -), НЕ в argv:
    // командная строка процесса видна другим процессам пользователя.
    let out = net::curl_secret_with_proxy(cmd, &secret_headers, &s.proxy_url)
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;
    parse_curl_chat(out, timeout_s)
}

fn parse_curl_chat(out: std::process::Output, timeout_s: u64) -> Result<String> {
    if !out.status.success() && out.stdout.is_empty() {
        if net::curl_timed_out(&out.status) {
            return Err(anyhow!(
                "Модель рерайта не ответила за {timeout_s} с. Увеличьте таймаут ИИ в настройках"
            ));
        }
        // curl упал без тела (сеть/нет прокси) — stderr безопасен (без ключа).
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("curl завершился с ошибкой: {}", err.trim()));
    }

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("ответ рерайта — не JSON: {e}"))?;

    let content = parse_chat_completion(&v)?;
    if content.trim().is_empty() {
        // Пустой ответ без поля error — отдаём диагностику без ключа.
        log::warn!("Rewrite вернул пустой текст; raw len={}", out.stdout.len());
        return Err(anyhow!(
            "OpenAI-совместимый рерайт вернул пустой ответ (нет текста в choices[0].message.content)"
        ));
    }
    Ok(content)
}

/// Текст из ответа /chat/completions. `finish_reason == "length"` означает, что
/// ответ обрублен лимитом токенов — такой результат отвергаем, иначе в поле
/// уходит половина фразы.
fn parse_chat_completion(v: &serde_json::Value) -> Result<String> {
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

    let choice = v.get("choices").and_then(|c| c.get(0));
    if let Some(reason) = choice
        .and_then(|c| c.get("finish_reason"))
        .and_then(|r| r.as_str())
    {
        if reason.eq_ignore_ascii_case("length") {
            return Err(anyhow!(
                "Модель оборвала ответ по лимиту токенов (finish_reason=length) — вставляем текст после правил"
            ));
        }
    }

    Ok(choice
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string())
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
    fn model_catalog_accepts_openai_and_gemini_shapes() {
        let openai = serde_json::json!({ "data": [ { "id": "gpt-4o-mini" }, { "id": "gpt-4o" } ] });
        assert_eq!(
            super::model_ids_from_catalog(&openai),
            vec!["gpt-4o", "gpt-4o-mini"]
        );
        let gemini = serde_json::json!({ "models": [ { "name": "models/gemini-2.5-flash" } ] });
        assert_eq!(
            super::model_ids_from_catalog(&gemini),
            vec!["gemini-2.5-flash"]
        );
        let dup = serde_json::json!({ "data": [ { "id": "a" }, { "id": "a" }, { "id": "" } ] });
        assert_eq!(super::model_ids_from_catalog(&dup), vec!["a"]);
        assert!(super::model_ids_from_catalog(&serde_json::json!({})).is_empty());
    }

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
    fn custom_header_name_replaces_bearer_convention() {
        assert_eq!(auth_header_line("", "k"), "Authorization: Bearer k");
        assert_eq!(auth_header_line(" ", "k"), "Authorization: Bearer k");
        assert_eq!(
            auth_header_line("Authorization", "k"),
            "Authorization: Bearer k"
        );
        assert_eq!(auth_header_line("x-api-key", "k"), "x-api-key: k");
    }

    #[test]
    fn truncated_answer_is_rejected_instead_of_inserted() {
        let truncated = serde_json::json!({
            "choices": [{
                "message": { "content": "Половина фра" },
                "finish_reason": "length"
            }]
        });
        assert!(parse_chat_completion(&truncated).is_err());

        let complete = serde_json::json!({
            "choices": [{
                "message": { "content": "Полная фраза." },
                "finish_reason": "stop"
            }]
        });
        assert_eq!(parse_chat_completion(&complete).unwrap(), "Полная фраза.");
    }

    #[test]
    fn output_budget_scales_with_input_and_respects_cap() {
        let short = net::output_token_budget(net::estimate_tokens("привет"), 4096);
        let long = net::output_token_budget(net::estimate_tokens(&"слово ".repeat(2000)), 4096);
        assert!(long > short);
        assert!(long <= 4096);
        assert!(short >= 256);
    }

    #[test]
    fn mandatory_reasoning_error_is_recognized() {
        let e = anyhow!(
            "Rewrite API error: Reasoning is mandatory for this endpoint and cannot be disabled."
        );
        assert!(reasoning_is_mandatory(&e));
        assert!(!reasoning_is_mandatory(&anyhow!(
            "Rewrite API error: invalid model"
        )));
    }

    fn or_model(
        id: &str,
        prompt: &str,
        completion: &str,
        out: &str,
        ready: bool,
    ) -> OpenRouterModel {
        OpenRouterModel {
            id: id.into(),
            name: Some(id.to_uppercase()),
            pricing: Some(OpenRouterPricing {
                prompt: Some(prompt.into()),
                completion: Some(completion.into()),
            }),
            output_modalities: vec![out.into()],
            is_ready: Some(ready),
            is_free: None,
        }
    }

    #[test]
    fn openrouter_catalog_lists_paid_text_models_cheapest_first() {
        let models = vec![
            or_model("big/expensive", "0.000003", "0.000015", "text", true),
            or_model("deepseek/deepseek-r1:free", "0", "0", "text", true),
            or_model("small/cheap", "0.00000005", "0.0000002", "text", true),
            or_model(
                OPENROUTER_RECOMMENDED_MODEL,
                "0.0000001",
                "0.0000005",
                "text",
                true,
            ),
            or_model("small/cheap:batch", "0.00000002", "0.0000001", "text", true),
            or_model("image/model", "0.000001", "0", "image", true),
            or_model("not/ready", "0.000001", "0.000001", "text", false),
            or_model("openrouter/auto", "-1", "-1", "text", true),
        ];

        let out = text_model_options(models);
        let ids: Vec<&str> = out.iter().map(|m| m.value.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                OPENROUTER_RECOMMENDED_MODEL,
                "small/cheap",
                "big/expensive",
                "deepseek/deepseek-r1:free",
            ]
        );
        assert!(out[0].label.ends_with("$0.10 / $0.50 за 1M · рекомендуем"));
        assert_eq!(out[1].label, "SMALL/CHEAP · $0.05 / $0.20 за 1M");
        assert!(out[3].label.ends_with("бесплатно, с лимитами"));
    }
}
