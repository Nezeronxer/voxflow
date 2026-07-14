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

/// A dead route/proxy must fail over quickly instead of consuming the whole
/// transcription deadline before local STT can start.
const CONNECT_TIMEOUT_SECS: &str = "3";
/// Ниже этого порога Deepgram чаще возвращает шумовую догадку. Пустой
/// результат позволит общему пайплайну безопасно уйти в local fallback.
const MIN_DEEPGRAM_CONFIDENCE: f64 = 0.45;
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

/// Best-effort cloud preview. It intentionally has a much shorter deadline
/// than the final request: after key release its result is stale and the final
/// transcription must not remain coupled to a detached preview process.
pub fn transcribe_partial(s: &Settings, wav: &Path) -> Result<String> {
    transcribe_with_profile(s, wav, None, RequestProfile::LiveDraft)
}

/// Распознать WAV с необязательным ASR-prompt.
///
/// Prompt используется только там, где провайдер поддерживает biasing напрямую
/// через совместимый `/audio/transcriptions` API. Сейчас это OpenAI-compatible
/// путь. Deepgram оставлен без prompt, потому что у него другой механизм biasing
/// (keywords/keyterms) и его нельзя слепо подменять текстовой подсказкой.
pub fn transcribe_with_prompt(s: &Settings, wav: &Path, prompt: Option<&str>) -> Result<String> {
    transcribe_with_profile(s, wav, prompt, RequestProfile::Final)
}

fn transcribe_with_profile(
    s: &Settings,
    wav: &Path,
    prompt: Option<&str>,
    profile: RequestProfile,
) -> Result<String> {
    match s.stt_provider.as_str() {
        "openai_compat" => match profile {
            RequestProfile::Final => transcribe_openai_compat(s, wav, prompt),
            RequestProfile::LiveDraft => transcribe_openai_compat_inner(s, wav, prompt, profile),
        },
        "deepgram" => match profile {
            RequestProfile::Final => transcribe_deepgram(s, wav),
            RequestProfile::LiveDraft => transcribe_deepgram_inner(s, wav, profile),
        },
        other => Err(anyhow!("неизвестный облачный STT-провайдер: {other}")),
    }
}

#[derive(Clone, Copy)]
enum RequestProfile {
    Final,
    LiveDraft,
}

fn request_timeout_for_audio(audio_seconds: u64, profile: RequestProfile) -> u64 {
    match profile {
        RequestProfile::LiveDraft => 8,
        RequestProfile::Final => 15u64
            .saturating_add(audio_seconds.saturating_mul(2))
            .clamp(20, 60),
    }
}

fn request_timeout_secs(wav: &Path, profile: RequestProfile) -> u64 {
    request_timeout_for_audio(
        crate::audio::wav_duration_secs_ceil(wav).unwrap_or(10),
        profile,
    )
}

/// OpenAI-совместимый STT (Avalon/OpenAI/Groq): multipart POST на
/// `{base}/audio/transcriptions`. Ответ — JSON `{"text":"..."}`.
pub fn transcribe_openai_compat(s: &Settings, wav: &Path, prompt: Option<&str>) -> Result<String> {
    transcribe_openai_compat_inner(s, wav, prompt, RequestProfile::Final)
}

fn transcribe_openai_compat_inner(
    s: &Settings,
    wav: &Path,
    prompt: Option<&str>,
    profile: RequestProfile,
) -> Result<String> {
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
        .arg("--connect-timeout")
        .arg(CONNECT_TIMEOUT_SECS)
        .arg("-m")
        .arg(request_timeout_secs(wav, profile).to_string())
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

    // Явный нуль не даёт compatible-провайдеру поднимать температуру и
    // «додумывать» текст на шумном конце фразы.
    cmd.arg("-F")
        .arg("temperature=0")
        .arg("-F")
        .arg("response_format=json");
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
    Ok(crate::asr::sanitize_transcript_text(text))
}

/// Deepgram: бинарный POST на `{base}/v1/listen?...`. Ответ — JSON, транскрипт
/// в `results.channels[0].alternatives[0].transcript`.
pub fn transcribe_deepgram(s: &Settings, wav: &Path) -> Result<String> {
    transcribe_deepgram_inner(s, wav, RequestProfile::Final)
}

fn transcribe_deepgram_inner(s: &Settings, wav: &Path, profile: RequestProfile) -> Result<String> {
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
        .arg("--connect-timeout")
        .arg(CONNECT_TIMEOUT_SECS)
        .arg("-m")
        .arg(request_timeout_secs(wav, profile).to_string())
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
    parse_deepgram_response(&body)
}

fn parse_deepgram_response(body: &str) -> Result<String> {
    let v: serde_json::Value =
        serde_json::from_str(body.trim()).map_err(|_| anyhow!("deepgram STT: ответ не JSON"))?;

    // Deepgram при ошибке отдаёт {"err_code":..,"err_msg":..} или {"error":..}.
    if let Some(msg) = v.get("err_msg").and_then(|m| m.as_str()) {
        return Err(anyhow!("deepgram STT: {msg}"));
    }
    if let Some(msg) = v.get("error").and_then(|m| m.as_str()) {
        return Err(anyhow!("deepgram STT: {msg}"));
    }

    // Безопасная навигация по results.channels[0].alternatives[0].
    let alternative = v
        .get("results")
        .and_then(|r| r.get("channels"))
        .and_then(|c| c.get(0))
        .and_then(|ch| ch.get("alternatives"))
        .and_then(|a| a.get(0))
        .ok_or_else(|| anyhow!("deepgram STT: нет альтернативы в ответе"))?;
    let transcript = alternative
        .get("transcript")
        .and_then(|t| t.as_str())
        .ok_or_else(|| anyhow!("deepgram STT: нет транскрипта в ответе"))?;
    let text = crate::asr::sanitize_transcript_text(transcript);
    if text.is_empty() {
        return Ok(text);
    }
    if alternative
        .get("confidence")
        .and_then(|c| c.as_f64())
        .is_some_and(|confidence| confidence < MIN_DEEPGRAM_CONFIDENCE)
    {
        log::info!("deepgram STT: низкая confidence, текст отклонён");
        return Ok(String::new());
    }
    Ok(text)
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

    #[test]
    fn cloud_deadlines_keep_final_quality_budget_but_bound_stale_drafts() {
        assert_eq!(request_timeout_for_audio(1, RequestProfile::Final), 20);
        assert_eq!(request_timeout_for_audio(5, RequestProfile::Final), 25);
        assert_eq!(request_timeout_for_audio(30, RequestProfile::Final), 60);
        assert_eq!(request_timeout_for_audio(600, RequestProfile::Final), 60);
        assert_eq!(request_timeout_for_audio(600, RequestProfile::LiveDraft), 8);
    }

    #[test]
    fn deepgram_low_confidence_becomes_empty_for_local_fallback() {
        let response = serde_json::json!({
            "results": {"channels": [{"alternatives": [{
                "transcript": "Yeah",
                "confidence": 0.12
            }]}]}
        })
        .to_string();
        assert!(parse_deepgram_response(&response).unwrap().is_empty());
    }

    #[test]
    fn deepgram_confident_text_is_normalized_without_rewrite() {
        let response = serde_json::json!({
            "results": {"channels": [{"alternatives": [{
                "transcript": "  Привет,   VoxFlow!  ",
                "confidence": 0.94
            }]}]}
        })
        .to_string();
        assert_eq!(
            parse_deepgram_response(&response).unwrap(),
            "Привет, VoxFlow!"
        );
    }
}
