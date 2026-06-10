//! Сетевые помощники: единая прокси-aware обёртка над системным curl.
//!
//! Все внешние HTTP-вызовы в проекте идут через системный `curl` (reqwest выкинут —
//! тянул rustls→aws-lc, нужен cmake). Этот модуль централизует поддержку прокси,
//! чтобы STT-облако и LLM-рефайн одинаково умели ходить через прокси из РФ.

use std::process::Command;

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

/// Есть ли вообще шанс выйти наружу: явный прокси в настройках ИЛИ в окружении.
/// (Диагностика; не блокирует — прямое соединение тоже валидно.)
pub fn proxy_configured(settings_proxy: &str) -> bool {
    if !settings_proxy.trim().is_empty() {
        return true;
    }
    ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]
        .iter()
        .any(|k| std::env::var(k).map(|v| !v.trim().is_empty()).unwrap_or(false))
}
