//! Клиент Google Gemini (Google AI Studio) для облачного ASR и рефайна текста.
//!
//! Используется СИСТЕМНЫЙ `curl` (без reqwest). Два режима:
//!   1. [`transcribe`] — распознавание WAV (cloud ASR) через inline-аудио.
//!   2. [`refine`] — правка/стилизация текста (тон, орфография, пунктуация).
//!
//! API подтверждён по https://ai.google.dev/api/generate-content :
//!   * endpoint: POST /v1beta/models/{model}:generateContent
//!   * inline-аудио: parts[].inline_data { mime_type, data(base64) } — поддерживается
//!   * авторизация: HTTP-заголовок `x-goog-api-key` (ключ НЕ в URL — приватность)
//!   * быстрая flash-модель: gemini-2.5-flash
//!
//! ВАЖНО: api_key НИКОГДА не пишется в лог.

use anyhow::{anyhow, Result};
use base64::Engine;
use std::path::Path;

use crate::net;

/// Базовый адрес generateContent-эндпоинта (без модели).
const BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// Доступен ли облачный режим: ключ непустой (после trim).
pub fn available(api_key: &str) -> bool {
    !api_key.trim().is_empty()
}

/// Распознать WAV-файл через Gemini (cloud ASR). Возвращает только текст.
///
/// `language` — код/название языка для подсказки модели; "auto" = определить язык.
pub fn transcribe(
    api_key: &str,
    model: &str,
    wav: &Path,
    language: &str,
    proxy_url: &str,
) -> Result<String> {
    // Читаем WAV и кодируем в base64.
    let bytes = std::fs::read(wav)
        .map_err(|e| anyhow!("не удалось прочитать WAV {}: {e}", wav.display()))?;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

    let lang_hint = match language.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => {
            "Автоматически определи язык речи. Не переводи текст: сохрани язык оригинала."
        }
        "ru" | "russian" => "Язык речи: русский.",
        "en" | "english" => "Язык речи: English.",
        other => return transcribe_with_language_hint(api_key, model, wav, &b64, other, proxy_url),
    };
    let prompt = format!(
        "Транскрибируй это аудио ДОСЛОВНО. {lang_hint} \
         Верни ТОЛЬКО распознанный текст, без кавычек и комментариев."
    );

    let body = serde_json::json!({
        "contents": [{
            "parts": [
                { "text": prompt },
                { "inline_data": { "mime_type": "audio/wav", "data": b64 } }
            ]
        }],
        "generationConfig": { "temperature": 0 }
    });

    call(api_key, model, &body, proxy_url)
}

fn transcribe_with_language_hint(
    api_key: &str,
    model: &str,
    _wav: &Path,
    b64: &str,
    language: &str,
    proxy_url: &str,
) -> Result<String> {
    let prompt = format!(
        "Транскрибируй это аудио ДОСЛОВНО. Язык речи: {language}. \
         Не переводи текст. Верни ТОЛЬКО распознанный текст, без кавычек и комментариев."
    );
    let body = serde_json::json!({
        "contents": [{
            "parts": [
                { "text": prompt },
                { "inline_data": { "mime_type": "audio/wav", "data": b64 } }
            ]
        }],
        "generationConfig": { "temperature": 0 }
    });
    call(api_key, model, &body, proxy_url)
}

/// Отрефайнить текст: `system` (инструкция) + `user` (исходный текст)
/// склеиваются в один text-part через двойной перевод строки.
pub fn refine(
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    proxy_url: &str,
) -> Result<String> {
    let combined = format!("{system}\n\n{user}");

    let body = serde_json::json!({
        "contents": [{
            "parts": [ { "text": combined } ]
        }],
        "generationConfig": { "temperature": 0.3 }
    });

    call(api_key, model, &body, proxy_url)
}

/// Общий вызов generateContent: пишет тело в temp-файл, дёргает curl,
/// парсит ответ и достаёт текст. Ключ передаётся ТОЛЬКО заголовком.
fn call(api_key: &str, model: &str, body: &serde_json::Value, proxy_url: &str) -> Result<String> {
    let url = format!("{BASE_URL}/{model}:generateContent");

    // Тело запроса — во временный файл (большой base64 не влезает в argv).
    let payload = serde_json::to_vec(body).map_err(|e| anyhow!("сериализация тела: {e}"))?;
    let req = net::TempPayload::write_json("gemini_req", &payload)?;
    let data_arg = req.curl_data_arg();
    let auth_header = format!("x-goog-api-key: {api_key}");

    // Прокси-aware curl из общего модуля net (CREATE_NO_WINDOW уже внутри).
    // Явный proxy_url передаём через stdin-config вместе с секретным заголовком,
    // чтобы user:pass@proxy не появлялся в argv.
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
        .arg(&url);

    // Ключ (x-goog-api-key) — через stdin-конфиг curl (-K -), НЕ в argv:
    // командная строка процесса видна другим процессам пользователя.
    let out = net::curl_secret_with_proxy(cmd, &[auth_header], proxy_url)
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

    if !out.status.success() && out.stdout.is_empty() {
        // curl упал без тела (сеть/таймаут) — stderr безопасен (без ключа).
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("curl завершился с ошибкой: {}", err.trim()));
    }

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("ответ Gemini — не JSON: {e}"))?;

    // Явная ошибка API.
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("неизвестная ошибка Gemini");
        return Err(anyhow!("Gemini error: {msg}"));
    }

    // candidates[0].content.parts[*].text — конкатенируем все текстовые куски.
    let parts = v
        .get("candidates")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("content"))
        .and_then(|c| c.get("parts"))
        .and_then(|p| p.as_array());

    let mut text = String::new();
    if let Some(parts) = parts {
        for part in parts {
            if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                text.push_str(t);
            }
        }
    }

    let trimmed = text.trim();
    if trimmed.is_empty() {
        // Возможно сработал safety/блок без поля error — отдаём диагностику без ключа.
        log::warn!("Gemini вернул пустой текст; raw len={}", out.stdout.len());
        return Err(anyhow!(
            "Gemini вернул пустой ответ (нет текста в candidates)"
        ));
    }

    Ok(trimmed.to_string())
}
