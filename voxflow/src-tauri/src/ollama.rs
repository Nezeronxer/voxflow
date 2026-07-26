//! Клиент локального Ollama (http://localhost:11434) для офлайн-рефайна текста.
//!
//! Аналог [`crate::gemini`], но БЕЗ ASR: текстовая модель Qwen3 правит только
//! стиль/орфографию/пунктуацию. Работает РЯДОМ с Gemini (выбор — `ai_backend`).
//!
//! Используется СИСТЕМНЫЙ `curl` (без reqwest — на машине нет cmake). Два метода:
//!   1. [`list_models`] — список установленных моделей (GET /api/tags).
//!   2. [`refine`] — правка/стилизация текста (POST /api/chat, stream=false).
//!
//! Особенность Qwen3 (гибридная reasoning-модель): размышления глушим тройным
//! способом — директивой `/no_think` в системном сообщении, полем `"think": false`
//! в теле и пост-обрезкой блока `<think>…</think>` из ответа.
//!
//! Ключей/секретов тут нет (локальный сервер), в лог ничего приватного не пишем.

use anyhow::{anyhow, Result};

use crate::net;

/// Системный промпт для рефайна (тот же файл, что и у облачного слоя).
pub const SYSTEM_PROMPT: &str = include_str!("../prompts/voiceflow_ru.txt");

/// Доступен ли локальный режим: адрес непустой (после trim).
pub fn configured(url: &str) -> bool {
    !url.trim().is_empty()
}

/// Нормализует базовый адрес: trim + срез хвостового `/`
/// (`http://localhost:11434/` → `http://localhost:11434`).
fn base(url: &str) -> String {
    url.trim().trim_end_matches('/').to_string()
}

/// Список установленных моделей через GET /api/tags. Возвращает имена из
/// `models[*].name`. Если curl упал или ответ — не JSON, отдаёт понятную ошибку.
pub fn list_models(url: &str) -> Result<Vec<String>> {
    let base_url = base(url);
    net::ensure_https_or_loopback_base(&base_url, "Ollama URL")?;
    let endpoint = format!("{base_url}/api/tags");

    // Прокси-aware curl из общего модуля net (CREATE_NO_WINDOW уже внутри).
    // Ollama по умолчанию локальна (localhost), но через net::curl() env-прокси
    // (HTTPS_PROXY/HTTP_PROXY) подхватится автоматически для нелокальных адресов.
    let mut cmd = net::curl();
    net::apply_proxy(&mut cmd, "");
    cmd.arg("-s").arg("-m").arg("15").arg(&endpoint);

    let out = cmd
        .output()
        .map_err(|e| anyhow!("не удалось запустить curl: {e}"))?;

    if !out.status.success() && out.stdout.is_empty() {
        // curl упал без тела (сеть/таймаут/нет сервера) — stderr безопасен.
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("Ollama недоступна по {url}: {}", err.trim()));
    }

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| anyhow!("Ollama недоступна по {url}: ответ не JSON ({e})"))?;

    // Явная ошибка сервера.
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return Err(anyhow!("Ollama error: {err}"));
    }

    let models = v
        .get("models")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name").and_then(|n| n.as_str()))
                .map(|s| s.to_string())
                .collect::<Vec<String>>()
        })
        .unwrap_or_default();

    Ok(models)
}

/// Отрефайнить текст: `system` (инструкция) + `user` (исходный текст) через
/// POST /api/chat (stream=false). Размышления гибридной qwen3 глушим
/// `/no_think` + `"think": false`, остаток `<think>…</think>` срезаем из ответа.
pub fn refine(url: &str, model: &str, system: &str, user: &str, timeout_s: u64) -> Result<String> {
    let base_url = base(url);
    net::ensure_https_or_loopback_base(&base_url, "Ollama URL")?;
    let endpoint = format!("{base_url}/api/chat");

    // ВАЖНО: директиву /no_think в системном сообщении НЕ добавляем — у qwen3:4b она
    // reasoning не глушит, а наоборот протекает эхом литералом «/no_think» в ответ.
    // Глушим только нативным `think: false` + пост-очисткой (strip_think +
    // looks_like_reasoning). Систему отдаём как есть.
    let system_msg = system.to_string();

    // Окно и лимит вывода считаем от фактической длины запроса. Хардкод 4096 был
    // МЕНЬШЕ одного системного промпта (~21 КБ ≈ 6k токенов): Ollama молча
    // срезала его начало, а на генерацию места не оставалось.
    let (num_ctx, num_predict) = plan_context_budget(&system_msg, user);

    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system_msg },
            { "role": "user",   "content": user }
        ],
        "stream": false,
        "think": false,
        "options": {
            "temperature": 0.2,   // было 0.7 — детерминизм рефайна, меньше отсебятины
            "top_p": 0.8,
            "top_k": 20,
            "min_p": 0.0,
            "num_ctx": num_ctx,
            "num_predict": num_predict
        }
    });

    // Тело запроса — во временный файл (как в gemini.rs): не упираемся в argv.
    let payload = serde_json::to_vec(&body).map_err(|e| anyhow!("сериализация тела: {e}"))?;
    let req = net::TempPayload::write_json("ollama_req", &payload)?;
    let data_arg = req.curl_data_arg();

    // Прокси-aware curl из общего модуля net (CREATE_NO_WINDOW уже внутри).
    // Локальный Ollama обычно прямой; пустой proxy → net::apply_proxy не добавляет -x,
    // curl сам читает env-прокси. Тройное глушение reasoning (см. тело body выше) и
    // strip_think в обработке ответа сохранены без изменений.
    let mut cmd = net::curl();
    net::apply_proxy(&mut cmd, "");
    cmd.arg("-s")
        .arg("-m")
        // Таймаут задаёт вызывающий (настройка `ai_timeout_s`, для локальной
        // модели не меньше 60 с). Фиксированные 10 с означали, что qwen3:4b на
        // CPU не успевал НИКОГДА — рефайна де-факто не было.
        .arg(timeout_s.to_string())
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
        if net::curl_timed_out(&out.status) {
            return Err(anyhow!(
                "Ollama не ответила за {timeout_s} с (модель {model}). Увеличьте таймаут ИИ в настройках или возьмите модель полегче"
            ));
        }
        // curl упал без тела (сеть/нет сервера) — stderr безопасен.
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(anyhow!("Ollama недоступна по {url}: {}", err.trim()));
    }

    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| anyhow!("ответ Ollama — не JSON: {e}"))?;

    let content = parse_chat_response(&v)?;
    let cleaned = strip_think(content);
    // qwen3:4b порой вываливает chain-of-thought БЕЗ тегов <think> (монолог-рассуждение
    // о задаче). Если очищенный ответ выглядит как рассуждение/эхо промпта, а не как
    // переписанный текст — не инжектим монолог, а деградируем на текст после правил.
    if cleaned.is_empty() {
        log::warn!("Ollama вернул пустой текст; raw len={}", out.stdout.len());
        return Err(anyhow!(
            "Ollama вернул пустой ответ (нет текста в message.content)"
        ));
    }
    if looks_like_reasoning(&cleaned, user) {
        log::warn!("Ollama: ответ похож на рассуждение/эхо промпта — деградация на правила");
        return Err(anyhow!(
            "Ollama: ответ не похож на переписанный текст (рефайн пропущен)"
        ));
    }

    Ok(cleaned)
}

/// Окно контекста и лимит генерации под конкретный запрос.
/// Возвращает `(num_ctx, num_predict)`.
fn plan_context_budget(system: &str, user: &str) -> (u32, u32) {
    let input = net::estimate_tokens(system) + net::estimate_tokens(user);
    // Верхняя граница вывода локальной модели — тот же порядок, что у облака.
    let predict = net::output_token_budget(input, 4096);
    let needed = input.saturating_add(predict).saturating_add(512);
    (needed.max(16_384), predict)
}

/// Достать текст ответа чат-эндпоинта, отвергнув обрыв по лимиту токенов.
/// `done_reason == "length"` означает обрубок: вставлять его в поле нельзя,
/// это ровно тот «пропал кусок текста», из-за которого всё затевалось.
fn parse_chat_response(v: &serde_json::Value) -> Result<&str> {
    // Явная ошибка сервера (например, модель не установлена).
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        return Err(anyhow!("Ollama error: {err}"));
    }
    if v.get("done_reason").and_then(|d| d.as_str()) == Some("length") {
        return Err(anyhow!(
            "Ollama оборвала ответ по лимиту токенов (done_reason=length) — вставляем текст после правил"
        ));
    }
    Ok(v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .unwrap_or(""))
}

/// Эвристика «ответ — это рассуждение/эхо промпта, а не переписанный текст».
/// КОНСЕРВАТИВНА, чтобы НЕ срезать легитимный короткий результат (напр. «Хорошо,
/// договорились»): срабатывает только при структурных маркерах промпта ЛИБО когда
/// ответ заметно длиннее входа И содержит явные «процессные» фразы.
fn looks_like_reasoning(out: &str, input: &str) -> bool {
    let low = out.to_lowercase();
    // Структурные маркеры нашего payload — модель спарротила промпт вместо ответа.
    const STRUCT: &[&str] = &["[приложение]", "[диктовка]", "[окружение]", "/no_think"];
    if STRUCT.iter().any(|m| low.contains(m)) {
        return true;
    }
    // Мета-рассуждение о задаче: только если ответ В РАЗЫ длиннее входа И есть
    // «процессные» фразы (так короткий легитимный текст никогда не срежется).
    let out_len = out.chars().count();
    let in_len = input.chars().count().max(1);
    const PROC: &[&str] = &[
        "переписать этот",
        "перепишу",
        "исходный текст",
        "надиктованн",
        "let me",
        "i need to",
        "the user",
        "rewrite the",
    ];
    out_len > in_len * 2 && PROC.iter().any(|m| low.contains(m))
}

/// Срезает блоки размышлений `<think>…</think>` (на случай, если глушилки не
/// сработали и модель всё же подумала), затем общий `trim`.
///
/// Важный для qwen3 случай: chat-template сам подставляет открывающий `<think>`
/// в промпт, поэтому модель часто возвращает в `message.content` ТОЛЬКО
/// закрывающий `</think>` без пары (вид `"</think>\n\nреальный текст"` или
/// `"рассуждение</think>\n\nтекст"`). Такой бесхозный закрывающий тег тоже
/// обрабатываем — иначе литерал `</think>` (вместе с возможным рассуждением до
/// него) протёк бы в инжектируемый текст.
fn strip_think(text: &str) -> String {
    let mut s = text.to_string();
    // 1) Парные блоки <think>…</think> — вырезаем все по очереди.
    while let Some(start) = s.find("<think>") {
        match s[start..].find("</think>") {
            Some(end_rel) => {
                let end = start + end_rel + "</think>".len();
                s.replace_range(start..end, "");
            }
            // Открывающий без закрывающего — висячее рассуждение до конца строки.
            None => {
                s.truncate(start);
                break;
            }
        }
    }
    // 2) Бесхозный закрывающий </think> без открывающего (штатный вывод qwen3 в
    //    /no_think): всё ДО первого </think> включительно — это съеденный/пустой
    //    reasoning-блок, удаляем его.
    if let Some(end_rel) = s.find("</think>") {
        s.replace_range(0..end_rel + "</think>".len(), "");
    }
    // Эхо-литерал директивы: qwen3 иногда повторяет «/no_think» прямо в тексте — убираем.
    s = s.replace("/no_think", "");
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_fits_system_prompt_and_output() {
        let (num_ctx, num_predict) = plan_context_budget(SYSTEM_PROMPT, "[ДИКТОВКА]: привет");
        let input =
            net::estimate_tokens(SYSTEM_PROMPT) + net::estimate_tokens("[ДИКТОВКА]: привет");
        assert!(num_ctx >= 16_384, "окно меньше минимума: {num_ctx}");
        assert!(
            num_ctx > input + num_predict,
            "окно {num_ctx} не вмещает вход {input} + вывод {num_predict}"
        );
        assert!(num_predict >= 256);
    }

    #[test]
    fn long_input_grows_the_window_above_the_minimum() {
        let long = "слово ".repeat(20_000);
        let (num_ctx, num_predict) = plan_context_budget(SYSTEM_PROMPT, &long);
        assert!(num_ctx > 16_384);
        assert!(num_ctx > net::estimate_tokens(&long) + num_predict);
    }

    #[test]
    fn truncated_answer_is_rejected_instead_of_inserted() {
        let truncated = serde_json::json!({
            "message": { "content": "Половина фра" },
            "done_reason": "length"
        });
        assert!(parse_chat_response(&truncated).is_err());

        let complete = serde_json::json!({
            "message": { "content": "Полная фраза." },
            "done_reason": "stop"
        });
        assert_eq!(parse_chat_response(&complete).unwrap(), "Полная фраза.");
    }
}
