//! Сетевые помощники: единая прокси-aware обёртка над системным curl.
//!
//! Все внешние HTTP-вызовы в проекте идут через системный `curl` (reqwest выкинут —
//! тянул rustls→aws-lc, нужен cmake). Этот модуль централизует поддержку прокси,
//! чтобы STT-облако и LLM-рефайн одинаково умели ходить через прокси из РФ.

use std::io::Write;
use std::process::{Command, Output, Stdio};

/// Windows: не показывать консольное окно у дочернего curl.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Создать `curl`-команду с подавленным окном (Windows) — единая точка входа.
pub fn curl() -> Command {
    let mut c = Command::new("curl");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Добавить `-x <proxy>` к curl-команде, если `proxy` непустой.
///
/// Если пусто — НИЧЕГО не добавляем: curl сам читает `HTTPS_PROXY`/`HTTP_PROXY`
/// (и lowercase-варианты) из окружения. Явная настройка приложения имеет приоритет
/// над окружением (флаг `-x` перебивает env). Это критично для плавности из РФ:
/// без рабочего прокси облачный STT/LLM просто не достучатся.
pub fn apply_proxy(cmd: &mut Command, proxy: &str) {
    let p = proxy.trim();
    if !p.is_empty() {
        cmd.arg("-x").arg(p);
    }
}

/// Строка curl-конфига для секретного заголовка: `header = "Имя: значение"`.
///
/// Внутри кавычек curl понимает бэкслеш-escape: экранируем `\` и `"`, а также
/// CR/LF — перевод строки в значении иначе разорвал бы строку конфига и позволил
/// бы инъекцию произвольных директив (`url = ...`) через значение ключа.
pub fn secret_header_line(header: &str) -> String {
    let mut esc = String::with_capacity(header.len() + 16);
    for ch in header.chars() {
        match ch {
            '\\' => esc.push_str("\\\\"),
            '"' => esc.push_str("\\\""),
            '\n' => esc.push_str("\\n"),
            '\r' => esc.push_str("\\r"),
            c => esc.push(c),
        }
    }
    format!("header = \"{esc}\"")
}

/// Запустить curl, передав СЕКРЕТНЫЕ заголовки через stdin-конфиг (`-K -`),
/// а НЕ через argv: командная строка дочернего процесса видна любому процессу
/// пользователя (Task Manager/WMI/ProcessExplorer), API-ключ туда попадать
/// не должен — это часть контракта «ключ никогда не логируется».
///
/// `cmd` — команда, собранная через [`curl`] (CREATE_NO_WINDOW уже внутри) со
/// всеми НЕсекретными аргументами: URL, -F/-X/--data-binary, Content-Type,
/// `-x` прокси (прокси — не секрет, остаётся в argv, с `-K -` не конфликтует).
pub fn curl_secret(mut cmd: Command, secret_headers: &[String]) -> std::io::Result<Output> {
    cmd.arg("-K").arg("-");
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    {
        // Конфиг крошечный (строки заголовков) — в пайп влезает целиком, без
        // дедлока. Drop stdin обязателен: curl читает конфиг до EOF.
        let mut stdin = child.stdin.take().expect("stdin задан как piped выше");
        let mut config = String::new();
        for h in secret_headers {
            config.push_str(&secret_header_line(h));
            config.push('\n');
        }
        stdin.write_all(config.as_bytes())?;
    }
    child.wait_with_output()
}

/// Есть ли вообще шанс выйти наружу: явный прокси в настройках ИЛИ в окружении.
/// (Диагностика; не блокирует — прямое соединение тоже валидно.)
pub fn proxy_configured(settings_proxy: &str) -> bool {
    if !settings_proxy.trim().is_empty() {
        return true;
    }
    [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ]
    .iter()
    .any(|k| {
        std::env::var(k)
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_header_line_простой_заголовок() {
        assert_eq!(
            secret_header_line("Authorization: Bearer sk-abc123"),
            r#"header = "Authorization: Bearer sk-abc123""#
        );
    }

    #[test]
    fn secret_header_line_экранирует_кавычку_и_бэкслеш() {
        // Ключ с кавычкой и бэкслешем не должен разорвать кавычки конфига.
        assert_eq!(
            secret_header_line(r#"Authorization: Bearer a"b\c"#),
            r#"header = "Authorization: Bearer a\"b\\c""#
        );
    }

    #[test]
    fn secret_header_line_экранирует_переводы_строк() {
        // LF/CR в значении = попытка инъекции новой директивы конфига —
        // должны уйти как \n/\r внутри кавычек, а не как реальный перенос.
        let line = secret_header_line("X-Evil: a\nurl = \"http://evil\"\rb");
        assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
        assert_eq!(line, r#"header = "X-Evil: a\nurl = \"http://evil\"\rb""#);
    }

    /// РЕАЛЬНЫЙ transport-тест: секретный заголовок уходит через `-K -`, ответ
    /// HTTP приходит. Требует сети (прокси 127.0.0.1:10808) — поэтому ignore;
    /// запуск: cargo test --lib net:: -- --ignored
    #[test]
    #[ignore = "требует сети/прокси: cargo test --lib net:: -- --ignored"]
    fn curl_secret_реальный_запрос_через_прокси() {
        let null_dev = if cfg!(windows) { "NUL" } else { "/dev/null" };
        let mut cmd = curl();
        cmd.env("HTTPS_PROXY", "http://127.0.0.1:10808");
        cmd.arg("-s")
            .arg("-m")
            .arg("30")
            .arg("-o")
            .arg(null_dev)
            .arg("-w")
            .arg("%{http_code}")
            .arg("https://huggingface.co");
        let out = curl_secret(cmd, &["X-Test: voxflow-secret-transport".to_string()])
            .expect("spawn curl с -K - не должен падать");
        let code = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "curl код {:?}, stderr: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        // Любой валидный HTTP-статус (2xx/3xx) = transport через stdin-конфиг работает.
        let c = code.trim();
        assert!(
            c.starts_with('2') || c.starts_with('3'),
            "ожидали HTTP 2xx/3xx, получили {c:?}"
        );
    }
}
