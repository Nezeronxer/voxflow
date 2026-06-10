//! Клиент OpenAI-совместимого облачного рефайна текста (/chat/completions).
//!
//! Третий бэкенд рерайта РЯДОМ с [`crate::gemini`] и [`crate::ollama`]
//! (выбор — `ai_backend == "openai_compat"`). Текстовая модель правит только
//! стиль/орфографию/пунктуацию по тому же системному промпту, что и Ollama.
//!
//! Цель — любые OpenAI-совместимые провайдеры через единый /chat/completions:
//! Groq (`https://api.groq.com/openai/v1`), OpenAI, локальные/прокси-эндпоинты,
//! compat-обёртки над Claude/прочими. Различие только в `rewrite_base_url`,
//! `rewrite_model` и Bearer-ключе.
//!
//! Используется СИСТЕМНЫЙ `curl` (без reqwest — на машине нет cmake). Прокси —
//! через [`crate::net::apply_proxy`] с `s.proxy_url` (облако из РФ ходит через
//! настроенный прокси). Тело пишем во временный файл и шлём `--data-binary @file`,
//! как в [`crate::gemini`]/[`crate::ollama`], чтобы не упираться в длину argv.
//!
//! ВАЖНО: ключ (`Authorization: Bearer …`) НИКОГДА не пишется в лог.

use anyhow::{anyhow, Result};

use crate::net;

/// Нормализует базовый адрес: trim + срез хвостового `/`
/// (`https://api.groq.com/openai/v1/` → `https://api.groq.com/openai/v1`).
fn base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
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
/// [`crate::ollama::SYSTEM_PROMPT`], `user` = `build_voiceflow_payload(actx, text)`.
///
/// Ключ берём из [`crate::settings::Settings::resolve_rewrite_key`] (настройки →
/// env REWRITE_API_KEY → OPENAI_API_KEY). Пустой ключ → ошибка (без вызова сети).
pub fn refine(s: &crate::settings::Settings, system: &str, user: &str) -> Result<String> {
    // Ключ резолвим первым: пустой → не ходим в сеть, отдаём понятную ошибку.
    let key = s.resolve_rewrite_key();
    if key.trim().is_empty() {
        return Err(anyhow!(
            "OpenAI-совместимый рерайт: не задан ключ (rewrite_key / env REWRITE_API_KEY / OPENAI_API_KEY)"
        ));
    }

    let endpoint = format!("{}/chat/completions", base(&s.rewrite_base_url));

    let body = serde_json::json!({
        "model": s.rewrite_model.trim(),
        "messages": [
            { "role": "system", "content": system },
            { "role": "user",   "content": user }
        ],
        "temperature": 0.2,   // детерминизм рефайна, меньше отсебятины (как у Ollama)
        "stream": false
    });

    // Тело запроса — во временный файл (как в gemini.rs/ollama.rs): не упираемся в argv.
    let req_path = crate::paths::tmp_dir().join("rewrite_req.json");
    let payload = serde_json::to_vec(&body).map_err(|e| anyhow!("сериализация тела: {e}"))?;
    std::fs::write(&req_path, &payload)
        .map_err(|e| anyhow!("не удалось записать {}: {e}", req_path.display()))?;

    let data_arg = format!("@{}", req_path.display());
    let auth_header = format!("Authorization: Bearer {key}");

    // Прокси-aware curl из общего модуля net (CREATE_NO_WINDOW уже внутри).
    // Облако из РФ: прокси берём из настроек (s.proxy_url); пустой → net::apply_proxy
    // не добавляет -x, и curl сам читает HTTPS_PROXY/HTTP_PROXY из окружения.
    let mut cmd = net::curl();
    net::apply_proxy(&mut cmd, &s.proxy_url);
    cmd.arg("-s")
        .arg("-m")
        // Рефайн — СИНХРОННЫЙ шаг перед вставкой текста: дольше ~10с он
        // обесценивает диктовку (пользователь уже ждёт). Не успел — вставляем
        // текст после правил (graceful-деградация выше по стеку).
        .arg("10")
        .arg("-H")
        .arg(&auth_header)
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-X")
        .arg("POST")
        .arg("--data-binary")
        .arg(&data_arg)
        .arg(&endpoint);

    let out = cmd
        .output()
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

    if !out.status.success() && out.stdout.is_empty() {
        // curl упал без тела (сеть/таймаут/нет прокси) — stderr безопасен (без ключа).
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("curl завершился с ошибкой: {}", err.trim()));
    }

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow!("ответ рерайта — не JSON: {e}"))?;

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

    let trimmed = content.trim();
    if trimmed.is_empty() {
        // Пустой ответ без поля error — отдаём диагностику без ключа.
        log::warn!("Rewrite вернул пустой текст; raw len={}", out.stdout.len());
        return Err(anyhow!(
            "OpenAI-совместимый рерайт вернул пустой ответ (нет текста в choices[0].message.content)"
        ));
    }

    Ok(trimmed.to_string())
}
