//! Сетевые помощники: единая прокси-aware обёртка над системным curl.
//!
//! Все внешние HTTP-вызовы в проекте идут через системный `curl` (reqwest выкинут —
//! тянул rustls→aws-lc, нужен cmake). Этот модуль централизует поддержку прокси,
//! чтобы STT-облако и LLM-рефайн одинаково умели ходить через прокси из РФ.

use anyhow::{anyhow, Result};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// Windows: не показывать консольное окно у дочернего curl.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn curl_program() -> PathBuf {
    #[cfg(windows)]
    {
        let root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let system_curl = root.join("System32").join("curl.exe");
        if system_curl.exists() {
            return system_curl;
        }
        PathBuf::from("curl.exe")
    }
    #[cfg(not(windows))]
    {
        PathBuf::from("curl")
    }
}

/// Создать `curl`-команду с подавленным окном (Windows) — единая точка входа.
pub fn curl() -> Command {
    let mut c = Command::new(curl_program());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    c
}

/// Запретить curl использовать env/app proxy для локальных loopback-запросов.
pub fn apply_no_proxy(cmd: &mut Command) {
    cmd.arg("--noproxy").arg("*");
}

fn curl_config_line(option: &str, value: &str) -> String {
    let mut esc = String::with_capacity(value.len() + 16);
    for ch in value.chars() {
        match ch {
            '\\' => esc.push_str("\\\\"),
            '"' => esc.push_str("\\\""),
            '\n' => esc.push_str("\\n"),
            '\r' => esc.push_str("\\r"),
            c => esc.push(c),
        }
    }
    format!("{option} = \"{esc}\"")
}

fn proxy_config_line(proxy: &str) -> Option<String> {
    let p = proxy.trim();
    if p.is_empty() {
        None
    } else {
        Some(curl_config_line("proxy", p))
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct ParsedBaseUrl {
    scheme: String,
    host: String,
    has_userinfo: bool,
}

fn parse_base_url(raw: &str) -> Option<ParsedBaseUrl> {
    let trimmed = raw.trim();
    let (scheme, rest) = trimmed.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.trim().is_empty() {
        return None;
    }
    let (has_userinfo, host_port) = match authority.rsplit_once('@') {
        Some((_, host_port)) => (true, host_port),
        None => (false, authority),
    };
    let host = if let Some(stripped) = host_port.strip_prefix('[') {
        stripped.split_once(']')?.0.to_string()
    } else {
        host_port.split(':').next().unwrap_or("").to_string()
    };
    let host = host.trim().trim_matches(['[', ']']).to_ascii_lowercase();
    if host.is_empty() {
        return None;
    }
    Some(ParsedBaseUrl {
        scheme: scheme.to_ascii_lowercase(),
        host,
        has_userinfo,
    })
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false)
}

pub fn is_loopback_base_url(raw: &str) -> bool {
    parse_base_url(raw)
        .map(|p| is_loopback_host(&p.host))
        .unwrap_or(false)
}

/// URL that receives private text/audio or bearer keys must be HTTPS, except
/// loopback HTTP for local-only services such as Ollama.
pub fn ensure_https_or_loopback_base(raw: &str, label: &str) -> Result<()> {
    let parsed = parse_base_url(raw).ok_or_else(|| {
        anyhow!("{label}: укажите полный http(s) URL с хостом, например https://api.example/v1")
    })?;
    if parsed.has_userinfo {
        return Err(anyhow!(
            "{label}: логин/пароль в URL не поддерживаются; храните ключ в поле API-ключ"
        ));
    }
    match parsed.scheme.as_str() {
        "https" => Ok(()),
        "http" if is_loopback_host(&parsed.host) => Ok(()),
        "http" => Err(anyhow!(
            "{label}: http разрешён только для localhost/127.0.0.1/[::1]; для внешних провайдеров используйте https"
        )),
        _ => Err(anyhow!("{label}: поддерживаются только http или https URL")),
    }
}

pub fn ensure_loopback_base(raw: &str, label: &str) -> Result<()> {
    let parsed = parse_base_url(raw).ok_or_else(|| {
        anyhow!("{label}: укажите полный http(s) URL с хостом, например http://localhost:11434")
    })?;
    if parsed.has_userinfo {
        return Err(anyhow!("{label}: логин/пароль в URL не поддерживаются"));
    }
    if matches!(parsed.scheme.as_str(), "http" | "https") && is_loopback_host(&parsed.host) {
        Ok(())
    } else {
        Err(anyhow!(
            "{label}: разрешён только loopback URL (localhost/127.0.0.1/[::1])"
        ))
    }
}

fn trailing_url_part(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    let (_, rest) = trimmed.split_once("://")?;
    match rest.find(['/', '?', '#']) {
        Some(i) => Some(&rest[i..]),
        None => Some(""),
    }
}

fn is_origin_only_base(raw: &str) -> bool {
    match trailing_url_part(raw) {
        Some("") => true,
        Some(part) if part.starts_with('/') => part.chars().all(|c| c == '/'),
        _ => false,
    }
}

/// Provider bases used to build query strings must be scheme+authority only.
pub fn ensure_https_origin_base(raw: &str, label: &str) -> Result<()> {
    ensure_https_or_loopback_base(raw, label)?;
    if !is_origin_only_base(raw) {
        return Err(anyhow!(
            "{label}: укажите только origin без пути, query и fragment, например https://api.deepgram.com"
        ));
    }
    Ok(())
}

pub fn percent_encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.as_bytes() {
        match *b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(*b as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub fn strip_url_userinfo(raw: &str) -> String {
    let trimmed = raw.trim();
    if let Some((scheme, rest)) = trimmed.split_once("://") {
        let (authority, suffix) = match rest.find(['/', '?', '#']) {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        if let Some((_, host_port)) = authority.rsplit_once('@') {
            return format!("{scheme}://{host_port}{suffix}");
        }
        return trimmed.to_string();
    }
    if let Some((_, host_port)) = trimmed.rsplit_once('@') {
        return host_port.to_string();
    }
    trimmed.to_string()
}

pub struct TempPayload {
    guard: crate::paths::TempFileGuard,
}

impl TempPayload {
    pub fn write_json(prefix: &str, payload: &[u8]) -> Result<Self> {
        let path = crate::paths::unique_tmp_path(prefix, "json");
        std::fs::write(&path, payload)
            .map_err(|e| anyhow!("не удалось записать {}: {e}", path.display()))?;
        Ok(Self {
            guard: crate::paths::TempFileGuard::new(path),
        })
    }

    pub fn path(&self) -> &Path {
        self.guard.path()
    }

    pub fn curl_data_arg(&self) -> String {
        format!("@{}", self.path().display())
    }
}

/// Строка curl-конфига для секретного заголовка: `header = "Имя: значение"`.
///
/// Внутри кавычек curl понимает бэкслеш-escape: экранируем `\` и `"`, а также
/// CR/LF — перевод строки в значении иначе разорвал бы строку конфига и позволил
/// бы инъекцию произвольных директив (`url = ...`) через значение ключа.
pub fn secret_header_line(header: &str) -> String {
    curl_config_line("header", header)
}

pub fn curl_secret_with_proxy(
    cmd: Command,
    secret_headers: &[String],
    proxy: &str,
) -> std::io::Result<Output> {
    let config_lines = proxy_config_line(proxy).into_iter().collect::<Vec<_>>();
    curl_secret_with_config(cmd, secret_headers, &config_lines)
}

pub fn curl_secret_with_config(
    mut cmd: Command,
    secret_headers: &[String],
    config_lines: &[String],
) -> std::io::Result<Output> {
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
        for line in config_lines {
            config.push_str(line);
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

    #[test]
    fn proxy_config_line_экранирует_секретный_proxy_url() {
        let line = proxy_config_line("http://u:p@127.0.0.1:1080\nurl = \"http://evil\"")
            .expect("proxy line");
        assert!(!line.contains('\n') && !line.contains('\r'), "{line:?}");
        assert_eq!(
            line,
            r#"proxy = "http://u:p@127.0.0.1:1080\nurl = \"http://evil\"""#
        );
    }

    #[test]
    fn ensure_https_or_loopback_base_rejects_plaintext_remote() {
        assert!(ensure_https_or_loopback_base("https://api.groq.com/openai/v1", "x").is_ok());
        assert!(ensure_https_or_loopback_base("http://localhost:11434", "x").is_ok());
        assert!(ensure_https_or_loopback_base("http://127.0.0.1:11434", "x").is_ok());
        assert!(ensure_https_or_loopback_base("http://127.1.2.3:11434", "x").is_ok());
        assert!(ensure_https_or_loopback_base("http://[::1]:11434", "x").is_ok());
        assert!(ensure_https_or_loopback_base("http://api.example.test/v1", "x").is_err());
        assert!(ensure_https_or_loopback_base("http://127.0.0.1.evil.test/v1", "x").is_err());
        assert!(ensure_https_or_loopback_base("http://127.evil.test/v1", "x").is_err());
        assert!(ensure_https_or_loopback_base("ftp://api.example.test/v1", "x").is_err());
        assert!(
            ensure_https_or_loopback_base("https://user:pass@api.example.test/v1", "x").is_err()
        );
        assert!(is_loopback_base_url("http://localhost:11434"));
        assert!(is_loopback_base_url("http://127.0.0.1:11434"));
        assert!(is_loopback_base_url("http://[::1]:11434"));
        assert!(!is_loopback_base_url("http://127.0.0.1.evil.test:11434"));
        assert!(!is_loopback_base_url("https://api.example.test/v1"));
    }

    #[test]
    fn origin_base_rejects_query_and_path_injection() {
        assert!(ensure_https_origin_base("https://api.deepgram.com", "x").is_ok());
        assert!(ensure_https_origin_base("https://api.deepgram.com/", "x").is_ok());
        assert!(ensure_https_origin_base("https://api.deepgram.com/v1", "x").is_err());
        assert!(
            ensure_https_origin_base("https://api.deepgram.com?callback=https://evil", "x")
                .is_err()
        );
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(percent_encode_query_value("nova-3"), "nova-3");
        assert_eq!(
            percent_encode_query_value("nova-3&callback=https://evil.test/a b"),
            "nova-3%26callback%3Dhttps%3A%2F%2Fevil.test%2Fa%20b"
        );
    }

    #[test]
    fn strip_url_userinfo_removes_proxy_credentials() {
        assert_eq!(
            strip_url_userinfo("http://user:pass@127.0.0.1:1080"),
            "http://127.0.0.1:1080"
        );
        assert_eq!(
            strip_url_userinfo("https://u:p@example.test/path?q=1"),
            "https://example.test/path?q=1"
        );
    }

    #[test]
    fn temp_payload_is_unique_and_removed_on_drop() {
        let path;
        {
            let p = TempPayload::write_json("secret-test", br#"{"text":"private"}"#).unwrap();
            path = p.path().to_path_buf();
            assert!(
                path.exists(),
                "payload file should exist while guard is alive"
            );
            assert!(p.curl_data_arg().starts_with('@'));
        }
        assert!(!path.exists(), "payload file should be removed on drop");
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
        let out =
            curl_secret_with_config(cmd, &["X-Test: voxflow-secret-transport".to_string()], &[])
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
