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
/// Максимальный размер ASR-prompt для OpenAI-compatible STT.
///
/// Это не rewrite-инструкция, а короткий bias-подсказчик для имён, терминов,
/// предыдущего хвоста фразы и app-контекста. Большой prompt ухудшает latency и
/// может начать влиять на текст как rewrite, поэтому ограничиваем заранее.
const MAX_STT_PROMPT_CHARS: usize = 1200;

/// Распознать WAV через выбранного облачного провайдера.
///
/// Диспетчеризация по `s.stt_provider`: "openai_compat" | "deepgram".
/// Прочие значения (в т.ч. "local") → ошибка (вызывать облачную транскрипцию
/// при локальном провайдере — логическая ошибка вызывающего кода).
pub fn transcribe(s: &Settings, wav: &Path) -> Result<String> {
    transcribe_with_prompt(s, wav, None)
}

/// Распознать WAV с необязательным ASR-prompt.
///
/// Prompt используется только там, где провайдер поддерживает biasing напрямую
/// через совместимый `/audio/transcriptions` API. Сейчас это OpenAI-compatible
/// путь. Deepgram оставлен без prompt, потому что у него другой механизм biasing
/// (keywords/keyterms) и его нельзя слепо подменять текстовой подсказкой.
pub fn transcribe_with_prompt(s: &Settings, wav: &Path, prompt: Option<&str>) -> Result<String> {
    match s.stt_provider.as_str() {
        "openai_compat" => transcribe_openai_compat(s, wav, prompt),
        "deepgram" => transcribe_deepgram(s, wav),
        other => Err(anyhow!("неизвестный облачный STT-провайдер: {other}")),
    }
}

/// OpenAI-совместимый STT (Avalon/OpenAI/Groq): multipart POST на
/// `{base}/audio/transcriptions`. Ответ — JSON `{"text":"..."}`.
pub fn transcribe_openai_compat(s: &Settings, wav: &Path, prompt: Option<&str>) -> Result<String> {
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

    // language — только если язык задан явно. auto/all/any/multi = автоопределение:
    // параметр не шлём, чтобы модель могла свободно выбрать язык и не резать mixed speech.
    if let Some(language) = normalized_cloud_language(&s.language) {
        cmd.arg("-F").arg(format!("language={language}"));
    }

    // Важное отличие от rewrite prompt: это короткая подсказка ASR для терминов и
    // контекста. --form-string защищает от curl-семантики `@file`, если prompt
    // начинается с @ или содержит спецсимволы.
    if let Some(prompt) = sanitized_stt_prompt(prompt) {
        cmd.arg("--form-string").arg(format!("prompt={prompt}"));
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
    // language=multi при auto/all/any/multi (Deepgram-специфика мультиязычного
    // распознавания), иначе конкретный язык. model/smart_format/punctuate всегда.
    let lang = deepgram_language_param(&s.language);
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

fn normalized_cloud_language(language: &str) -> Option<&str> {
    let language = language.trim();
    if language.is_empty() {
        return None;
    }
    let lower = language.to_ascii_lowercase();
    match lower.as_str() {
        "auto" | "all" | "any" | "multi" | "multilingual" | "*" => None,
        _ => Some(language),
    }
}

fn deepgram_language_param(language: &str) -> String {
    normalized_cloud_language(language)
        .unwrap_or("multi")
        .to_string()
}

fn sanitized_stt_prompt(prompt: Option<&str>) -> Option<String> {
    let prompt = prompt?;
    let collapsed = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    let collapsed = collapsed.trim();
    if collapsed.is_empty() {
        return None;
    }
    Some(collapsed.chars().take(MAX_STT_PROMPT_CHARS).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloud_language_aliases_keep_auto_detection() {
        for lang in ["", "auto", "all", "any", "multi", "multilingual", "*"] {
            assert_eq!(normalized_cloud_language(lang), None, "{lang}");
        }
        assert_eq!(normalized_cloud_language("ru"), Some("ru"));
        assert_eq!(normalized_cloud_language(" es "), Some("es"));
    }

    #[test]
    fn deepgram_auto_aliases_use_multi() {
        assert_eq!(deepgram_language_param("auto"), "multi");
        assert_eq!(deepgram_language_param("all"), "multi");
        assert_eq!(deepgram_language_param("de"), "de");
    }

    #[test]
    fn stt_prompt_is_collapsed_and_capped() {
        let prompt = sanitized_stt_prompt(Some("  VoxFlow\nWispr\tFlow   Aqua Voice  ")).unwrap();
        assert_eq!(prompt, "VoxFlow Wispr Flow Aqua Voice");

        let long = "я".repeat(MAX_STT_PROMPT_CHARS + 50);
        let capped = sanitized_stt_prompt(Some(&long)).unwrap();
        assert_eq!(capped.chars().count(), MAX_STT_PROMPT_CHARS);
    }
}
