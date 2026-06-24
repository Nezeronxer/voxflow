//! Облачный STT-слой (v1, финальный проход, БЕЗ WebSocket).
//!
//! Две REST-реализации: OpenAI-совместимый (Avalon/OpenAI/Groq) и Deepgram.
//! Все HTTP идут через `net::curl()` + `net::curl_secret_with_proxy`, чтобы
//! ключи и прокси-учетные данные не попадали в аргументы процесса.
//!
//! ЖЁСТКО: ключ НИКОГДА не попадает в лог и не уходит в URL — только в заголовок
//! Authorization. Логируем лишь код/стадию ошибки.

use std::path::Path;

use anyhow::{anyhow, Result};

use crate::net;
use crate::settings::Settings;

/// Таймаут curl на облачный запрос (секунды).
const TIMEOUT_SECS: &str = "60";

/// Распознать WAV через выбранного облачного провайдера.
///
/// Диспетчеризация по `s.stt_provider`: "openai_compat" | "deepgram".
/// Прочие значения (в т.ч. "local") → ошибка (вызывать облачную транскрипцию
/// при локальном провайдере — логическая ошибка вызывающего кода).
pub fn transcribe(s: &Settings, wav: &Path) -> Result<String> {
    match s.stt_provider.as_str() {
        "openai_compat" => transcribe_openai_compat(s, wav),
        "deepgram" => transcribe_deepgram(s, wav),
        other => Err(anyhow!("неизвестный облачный STT-провайдер: {other}")),
    }
}

/// OpenAI-совместимый STT (Avalon/OpenAI/Groq): multipart POST на
/// `{base}/audio/transcriptions`. Ответ — JSON `{"text":"..."}`.
pub fn transcribe_openai_compat(s: &Settings, wav: &Path) -> Result<String> {
    let key = s.resolve_oai_key();
    if key.trim().is_empty() {
        return Err(anyhow!("ключ не задан"));
    }

    // Базовый URL без хвостового слэша, чтобы не получить двойной "//".
    let base = s.oai_stt_base_url.trim().trim_end_matches('/');
    net::ensure_https_or_loopback_base(base, "OpenAI-compatible STT Base URL")?;
    let url = format!("{base}/audio/transcriptions");

    let auth = format!("Authorization: Bearer {key}");
    let file_arg = format!("file=@{}", wav.display());
    let model_arg = format!("model={}", s.oai_stt_model);

    let mut cmd = net::curl();
    cmd.arg("-s")
        .arg("-m")
        .arg(TIMEOUT_SECS)
        .arg("-F")
        .arg(&file_arg)
        .arg("-F")
        .arg(&model_arg);
    // language — только если не "auto" (auto = автоопределение, параметр не шлём).
    if s.language != "auto" {
        cmd.arg("-F").arg(format!("language={}", s.language));
    }
    cmd.arg("-F").arg("response_format=json");
    cmd.arg(&url);

    // Ключ — через stdin-конфиг curl (-K -), НЕ в argv: командная строка
    // процесса видна другим процессам пользователя.
    let out = net::curl_secret_with_proxy(cmd, &[auth], &s.proxy_url)
        .map_err(|e| anyhow!("curl /audio/transcriptions: {e}"))?;
    if !out.status.success() {
        // НЕ логируем тело (могут быть заголовки/эхо); только код процесса curl.
        log::warn!("openai_compat STT: curl завершился с кодом {}", out.status);
        return Err(anyhow!("сеть/прокси недоступны (curl код {})", out.status));
    }

    let body = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(body.trim())
        .map_err(|_| anyhow!("openai_compat STT: ответ не JSON"))?;

    // Явная ошибка API ({"error": ...}) — отдаём наверх, текст ошибки без ключа.
    if let Some(err) = v.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("ошибка API");
        return Err(anyhow!("openai_compat STT: {msg}"));
    }

    let text = v
        .get("text")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("openai_compat STT: нет поля text"))?;
    Ok(text.trim().to_string())
}

/// Deepgram: бинарный POST на `{base}/v1/listen?...`. Ответ — JSON, транскрипт
/// в `results.channels[0].alternatives[0].transcript`.
pub fn transcribe_deepgram(s: &Settings, wav: &Path) -> Result<String> {
    let key = s.resolve_deepgram_key();
    if key.trim().is_empty() {
        return Err(anyhow!("ключ не задан"));
    }

    let base = s.deepgram_base.trim().trim_end_matches('/');
    net::ensure_https_or_loopback_base(base, "Deepgram Base URL")?;
    // language=multi при auto (Deepgram-специфика мультиязычного распознавания),
    // иначе конкретный язык. model/smart_format/punctuate всегда.
    let lang = if s.language == "auto" {
        "multi".to_string()
    } else {
        s.language.clone()
    };
    let url = format!(
        "{base}/v1/listen?model={}&smart_format=true&punctuate=true&language={}",
        s.deepgram_model, lang
    );

    let auth = format!("Authorization: Token {key}");
    let data_arg = format!("@{}", wav.display());

    let mut cmd = net::curl();
    cmd.arg("-s")
        .arg("-m")
        .arg(TIMEOUT_SECS)
        // Content-Type не секрет — остаётся в argv.
        .arg("-H")
        .arg("Content-Type: audio/wav")
        .arg("--data-binary")
        .arg(&data_arg);
    cmd.arg(&url);

    // Ключ — через stdin-конфиг curl (-K -), НЕ в argv (виден в Task Manager/WMI).
    let out = net::curl_secret_with_proxy(cmd, &[auth], &s.proxy_url)
        .map_err(|e| anyhow!("curl /v1/listen: {e}"))?;
    if !out.status.success() {
        log::warn!("deepgram STT: curl завершился с кодом {}", out.status);
        return Err(anyhow!("сеть/прокси недоступны (curl код {})", out.status));
    }

    let body = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value =
        serde_json::from_str(body.trim()).map_err(|_| anyhow!("deepgram STT: ответ не JSON"))?;

    // Deepgram при ошибке отдаёт {"err_code":..,"err_msg":..} или {"error":..}.
    if let Some(msg) = v.get("err_msg").and_then(|m| m.as_str()) {
        return Err(anyhow!("deepgram STT: {msg}"));
    }
    if let Some(msg) = v.get("error").and_then(|m| m.as_str()) {
        return Err(anyhow!("deepgram STT: {msg}"));
    }

    // Безопасная навигация по results.channels[0].alternatives[0].transcript.
    let transcript = v
        .get("results")
        .and_then(|r| r.get("channels"))
        .and_then(|c| c.get(0))
        .and_then(|ch| ch.get("alternatives"))
        .and_then(|a| a.get(0))
        .and_then(|alt| alt.get("transcript"))
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("deepgram STT: нет транскрипта в ответе"))?;
    Ok(transcript.trim().to_string())
}
