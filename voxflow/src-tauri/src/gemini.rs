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
    timeout_s: u64,
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
        other => return transcribe_with_language_hint(api_key, model, wav, &b64, other, timeout_s),
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

    call(api_key, model, &body, timeout_s)
}

fn transcribe_with_language_hint(
    api_key: &str,
    model: &str,
    _wav: &Path,
    b64: &str,
    language: &str,
    timeout_s: u64,
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
    call(api_key, model, &body, timeout_s)
}

/// Отрефайнить текст: `system` (инструкция) + `user` (исходный текст)
/// склеиваются в один text-part через двойной перевод строки.
pub fn refine(
    api_key: &str,
    model: &str,
    system: &str,
    user: &str,
    timeout_s: u64,
    max_output_tokens_cap: u32,
) -> Result<String> {
    let combined = format!("{system}\n\n{user}");
    // Лимит вывода считаем от длины входа: без него gemini-2.5-flash тратил
    // бюджет на размышления и обрывал ответ по MAX_TOKENS. Рерайт — это
    // форматирование, thinking тут не нужен и только съедает бюджет.
    let max_output_tokens =
        net::output_token_budget(net::estimate_tokens(&combined), max_output_tokens_cap);

    let body = serde_json::json!({
        "contents": [{
            "parts": [ { "text": combined } ]
        }],
        "generationConfig": {
            "temperature": 0.3,
            "maxOutputTokens": max_output_tokens,
            "thinkingConfig": { "thinkingBudget": 0 }
        }
    });

    call(api_key, model, &body, timeout_s)
}

/// Общий вызов generateContent: пишет тело в temp-файл, дёргает curl,
/// парсит ответ и достаёт текст. Ключ передаётся ТОЛЬКО заголовком.
fn call(api_key: &str, model: &str, body: &serde_json::Value, timeout_s: u64) -> Result<String> {
    let url = format!("{BASE_URL}/{model}:generateContent");

    // Тело запроса — во временный файл (большой base64 не влезает в argv).
    let payload = serde_json::to_vec(body).map_err(|e| anyhow!("сериализация тела: {e}"))?;
    let req = net::TempPayload::write_json("gemini_req", &payload)?;
    let data_arg = req.curl_data_arg();
    let auth_header = format!("x-goog-api-key: {api_key}");

    // Прокси-aware curl из общего модуля net (CREATE_NO_WINDOW уже внутри).
    // У публичных сигнатур transcribe/refine (их зовёт engine.rs) НЕТ proxy_url,
    // поэтому пробрасываем пустую строку: net::apply_proxy в этом случае НЕ добавляет
    // -x, и curl сам берёт HTTPS_PROXY/HTTP_PROXY из окружения. Так облачный путь из РФ
    // ходит через системный/env-прокси без смены чужого контракта engine.rs.
    let mut cmd = net::curl();
    net::apply_proxy(&mut cmd, "");
    cmd.arg("-s")
        .arg("-m")
        // Таймаут задаёт вызывающий (настройка `ai_timeout_s`). Не успел —
        // вставляем текст после правил (graceful-деградация выше по стеку).
        .arg(timeout_s.to_string())
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
    let out = net::curl_secret(cmd, &[auth_header])
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

    if !out.status.success() && out.stdout.is_empty() {
        if net::curl_timed_out(&out.status) {
            return Err(anyhow!(
                "Gemini не ответила за {timeout_s} с. Увеличьте таймаут ИИ в настройках"
            ));
        }
        // curl упал без тела (сеть) — stderr безопасен (без ключа).
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("curl завершился с ошибкой: {}", err.trim()));
    }

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("ответ Gemini — не JSON: {e}"))?;

    let text = parse_generate_content(&v)?;
    if text.is_empty() {
        // Возможно сработал safety/блок без поля error — отдаём диагностику без ключа.
        log::warn!("Gemini вернул пустой текст; raw len={}", out.stdout.len());
        return Err(anyhow!(
            "Gemini вернул пустой ответ (нет текста в candidates)"
        ));
    }

    Ok(text)
}

/// Текст из ответа generateContent. Обрыв по лимиту токенов
/// (`finishReason: "MAX_TOKENS"`) — это обрубок, а не результат: раньше части
/// просто склеивались и полфразы уходило в поле пользователя.
fn parse_generate_content(v: &serde_json::Value) -> Result<String> {
    // Явная ошибка API.
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("неизвестная ошибка Gemini");
        return Err(anyhow!("Gemini error: {msg}"));
    }

    let candidate = v.get("candidates").and_then(|c| c.get(0));
    if let Some(reason) = candidate
        .and_then(|c| c.get("finishReason"))
        .and_then(|r| r.as_str())
    {
        if reason.eq_ignore_ascii_case("MAX_TOKENS") {
            return Err(anyhow!(
                "Gemini оборвала ответ по лимиту токенов (finishReason=MAX_TOKENS) — вставляем текст после правил"
            ));
        }
    }

    // candidates[0].content.parts[*].text — конкатенируем все текстовые куски.
    let parts = candidate
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
    Ok(text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncated_answer_is_rejected_instead_of_inserted() {
        let truncated = serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": "Половина фра" }] },
                "finishReason": "MAX_TOKENS"
            }]
        });
        assert!(parse_generate_content(&truncated).is_err());

        let complete = serde_json::json!({
            "candidates": [{
                "content": { "parts": [{ "text": "Полная " }, { "text": "фраза." }] },
                "finishReason": "STOP"
            }]
        });
        assert_eq!(parse_generate_content(&complete).unwrap(), "Полная фраза.");
    }
}
