//! Определение контекста активного (переднего) приложения.
//!
//! Модуль узнаёт имя exe-файла процесса окна, находящегося на переднем плане,
//! а также заголовок этого окна (на Windows через сырые вызовы WinAPI без
//! сторонних крейтов), после чего классифицирует приложение в одну из
//! стилевых категорий (по приоритету): `"verbatim"`, `"ai"`, `"formal"`,
//! `"work"`, `"casual"`, `"doc"` или `"neutral"`.
//!
//! Маршрутизация задаётся ДАННО-УПРАВЛЯЕМОЙ таблицей правил в [`classify`]
//! (массив `(маркеры, профиль)`, проверяемый по порядку приоритета).
//! Пользователь может расширить/переопределить её своими правилами через
//! [`category_for`] (см. `Settings::app_profile_overrides`).
//!
//! В пользовательских правилах допустим псевдоним профиля `"code"` — это
//! удобное имя для случая «диктовка как есть, без переписывания»; внутренне
//! он отображается в `"verbatim"` (то же самое: rewrite выключен). См.
//! [`VALID_PROFILES`] и [`category_for`].
//!
//! На не-Windows платформах: на macOS — best-effort через `osascript`
//! (см. ниже), на остальных — пустой контекст с категорией `"neutral"`.

/// Контекст активного приложения.
///
/// * `exe` — имя исполняемого файла переднего окна в нижнем регистре
///   (например, `"chrome.exe"`); пустая строка, если определить не удалось.
/// * `title` — заголовок переднего окна; пустая строка при неудаче.
/// * `window_id` — стабильный идентификатор окна в рамках текущей сессии ОС
///   (на Windows это HWND); пустая строка, если платформа его не даёт.
/// * `category` — стилевая категория: `"verbatim" | "ai" | "formal" | "work"
///   | "casual" | "doc" | "neutral"`.
#[derive(Clone, Debug)]
pub struct AppContext {
    /// Имя exe-файла в нижнем регистре (только имя файла, без пути).
    pub exe: String,
    /// Заголовок переднего окна.
    pub title: String,
    /// Стабильный id foreground-окна, если платформа умеет его дать.
    pub window_id: String,
    /// Стилевая категория приложения.
    pub category: String,
}

/// Отпечаток целевого окна для безопасной отложенной вставки.
///
/// Когда доступен `window_id`, он надёжнее заголовка: браузеры и Electron-приложения
/// часто меняют title между окончанием записи и финальной вставкой, из-за чего
/// строгая проверка `(exe,title)` ложно отменяла вставку.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetFingerprint {
    exe: String,
    title: String,
    window_id: String,
}

impl AppContext {
    pub fn target_fingerprint(&self) -> TargetFingerprint {
        TargetFingerprint {
            exe: self.exe.clone(),
            title: self.title.clone(),
            window_id: self.window_id.clone(),
        }
    }
}

impl TargetFingerprint {
    pub fn describe(&self) -> String {
        format!(
            "exe={} title_len={} window_id={}",
            self.exe,
            self.title.chars().count(),
            self.window_id
        )
    }

    pub fn is_own_app(&self) -> bool {
        is_own_app_parts(&self.exe, &self.window_id)
    }

    pub fn is_transient_system_ui(&self) -> bool {
        is_transient_system_ui_parts(&self.exe, &self.window_id)
    }

    pub fn is_usable_dictation_target(&self) -> bool {
        !self.exe.is_empty() && !self.is_own_app() && !self.is_transient_system_ui()
    }

    #[cfg(target_os = "macos")]
    pub fn macos_pid(&self) -> Option<u32> {
        value_from_window_id(&self.window_id, "pid=")?.parse().ok()
    }

    #[cfg(target_os = "macos")]
    pub fn macos_bundle_id(&self) -> Option<String> {
        let bundle = value_from_window_id(&self.window_id, "bundle=")?;
        if bundle.is_empty() {
            None
        } else {
            Some(bundle.to_string())
        }
    }

    pub fn matches(&self, current: &AppContext) -> bool {
        if !self.window_id.is_empty() && !current.window_id.is_empty() {
            return self.exe == current.exe && self.window_id == current.window_id;
        }
        self.exe == current.exe && self.title == current.title
    }
}

impl AppContext {
    pub fn is_own_app(&self) -> bool {
        is_own_app_parts(&self.exe, &self.window_id)
    }

    pub fn is_transient_system_ui(&self) -> bool {
        is_transient_system_ui_parts(&self.exe, &self.window_id)
    }

    pub fn is_usable_dictation_target(&self) -> bool {
        !self.exe.is_empty() && !self.is_own_app() && !self.is_transient_system_ui()
    }
}

fn is_own_app_parts(exe: &str, window_id: &str) -> bool {
    exe.eq_ignore_ascii_case("voxflow")
        || exe.eq_ignore_ascii_case("voxflow.exe")
        || window_id.contains("bundle=com.nezeronxer.voxflow")
}

fn is_transient_system_ui_parts(exe: &str, window_id: &str) -> bool {
    const BUNDLES: &[&str] = &[
        "bundle=com.apple.UserNotificationCenter",
        "bundle=com.apple.accessibility.universalAccessAuthWarn",
        "bundle=com.apple.loginwindow",
        "bundle=com.apple.systempreferences",
        "bundle=com.apple.systemsettings",
        "bundle=com.apple.SystemSettings",
    ];
    const EXES: &[&str] = &[
        "usernotificationcenter",
        "universalaccessauthwarn",
        "loginwindow",
        "system preferences",
        "system settings",
        "systemsettings",
    ];
    let exe = exe.trim().to_ascii_lowercase();
    BUNDLES.iter().any(|b| window_id.contains(b))
        || EXES.iter().any(|name| exe == *name || exe.contains(name))
}

#[cfg(target_os = "macos")]
fn value_from_window_id<'a>(window_id: &'a str, key: &str) -> Option<&'a str> {
    let start = window_id.find(key)? + key.len();
    let rest = &window_id[start..];
    let end = rest.find(';').unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Классифицирует приложение по имени exe и заголовку окна.
///
/// Оба аргумента уже должны быть в нижнем регистре. Маршрутизация задана
/// ДАННО-УПРАВЛЯЕМОЙ таблицей правил `(точные exe, маркеры-подстроки, профиль)`,
/// проверяемой строго ПО ПОРЯДКУ приоритета сверху вниз. Побеждает первое
/// совпадение; если не совпало ничего — `"neutral"`.
///
/// Приоритет (важное — выше): `verbatim` (код/терминалы) → `ai` →
/// `formal` (почта/деловое) → `work` (рабочие сервисы) → `casual` (личные
/// мессенджеры) → `doc` (документы) → `neutral`.
///
/// Это ВСТРОЕННАЯ таблица БЕЗ учёта пользовательских правил. Для классификации
/// с учётом `Settings::app_profile_overrides` используйте [`category_for`].
pub fn classify(exe: &str, title: &str) -> String {
    // Удобная замыкающая проверка: содержит ли exe ИЛИ title подстроку.
    let any = |needle: &str| exe.contains(needle) || title.contains(needle);

    // Правило: (точные имена exe, маркеры-подстроки в exe/title, имя профиля).
    // Порядок элементов = порядок приоритета (см. docstring).
    struct Rule {
        exes: &'static [&'static str],
        markers: &'static [&'static str],
        profile: &'static str,
    }

    let rules: &[Rule] = &[
        // 1) verbatim — редакторы кода и терминалы (код важнее всего).
        Rule {
            exes: &[
                "code.exe",
                "cursor.exe",
                "windowsterminal.exe",
                "powershell.exe",
                "cmd.exe",
                "wt.exe",
                "alacritty.exe",
                "conhost.exe",
            ],
            markers: &[
                "visual studio code",
                " - cursor",
                "jetbrains",
                "intellij",
                "pycharm",
                "webstorm",
                "rider",
                "goland",
                "clion",
                "phpstorm",
                "rubymine",
                "android studio",
                "sublime text",
                "neovim",
                "vim",
            ],
            profile: "verbatim",
        },
        // 2) ai — ассистенты и связанные сервисы.
        Rule {
            exes: &[],
            markers: &[
                "chatgpt",
                "chat.openai",
                "openai",
                "codex",
                "claude.ai",
                "claude",
                "gemini.google",
                "gemini",
                "bard",
                "perplexity",
                "copilot",
                "deepseek",
                "you.com",
                "poe",
                "huggingface",
                "t3.chat",
                "x.ai",
                "grok",
                "mistral",
                "le chat",
            ],
            profile: "ai",
        },
        // 3) formal — почтовые клиенты и деловое.
        Rule {
            exes: &["outlook.exe", "thunderbird.exe"],
            markers: &[
                "gmail",
                "outlook",
                "почт",
                " mail",
                "proton",
                "protonmail",
                "spark",
                "mail.ru",
                "linkedin",
                "jira",
                "yandex mail",
                "яндекс почт",
                "zoho mail",
            ],
            profile: "formal",
        },
        // 4) work — рабочие сервисы/коллаборация.
        Rule {
            exes: &[],
            markers: &[
                "slack",
                "microsoft teams",
                " teams",
                "notion",
                "confluence",
                "trello",
                "asana",
                "mattermost",
                "webex",
            ],
            profile: "work",
        },
        // 5) casual — личные мессенджеры и соцсети.
        Rule {
            exes: &["telegram.exe", "discord.exe"],
            markers: &[
                "telegram",
                "web.telegram",
                "whatsapp",
                "signal",
                "discord",
                "viber",
                "vk.com",
                "вконтакте",
                "messenger",
                "skype",
                "max ",
                "ватсап",
            ],
            profile: "casual",
        },
        // 6) doc — текстовые документы.
        Rule {
            exes: &["winword.exe", "pages.app"],
            markers: &[
                "docs.google",
                "google docs",
                "- word",
                "microsoft word",
                " — pages",
                "onlyoffice",
                "libreoffice writer",
            ],
            profile: "doc",
        },
    ];

    for rule in rules {
        if rule.exes.contains(&exe) || rule.markers.iter().any(|m| any(m)) {
            return rule.profile.to_string();
        }
    }

    // По умолчанию — нейтральный стиль.
    "neutral".to_string()
}

/// Допустимые имена профилей (для валидации пользовательских правил).
///
/// `"code"` — псевдоним: для пользователя это понятное имя «диктовать как
/// есть», но внутренне он отображается в `"verbatim"` (см. [`category_for`]),
/// чтобы сработал существующий verbatim-гейт движка (rewrite выключен).
const VALID_PROFILES: &[&str] = &[
    "verbatim", "code", "ai", "formal", "work", "casual", "doc", "neutral",
];

/// Классифицирует приложение с учётом ПОЛЬЗОВАТЕЛЬСКИХ переопределений.
///
/// Сначала по порядку проверяются `overrides`: если `pattern` (в нижнем
/// регистре) встречается как подстрока в `exe` ИЛИ `title` (тоже приводятся
/// к нижнему регистру), и профиль правила валиден — возвращается этот профиль.
/// Псевдоним `"code"` при этом отображается в `"verbatim"` (rewrite выключен).
/// Невалидные правила (профиль вне множества `{verbatim, code, ai, formal,
/// work, casual, doc, neutral}` или пустой `pattern`) пропускаются; при этом
/// в лог пишется краткое предупреждение (`log::warn!`, без секретов), чтобы
/// проблему было видно при диагностике.
///
/// Если ни одно пользовательское правило не сработало — результат
/// определяет встроенная таблица [`classify`].
///
/// Аргументы могут приходить в любом регистре: нормализация выполняется внутри.
pub fn category_for(
    exe: &str,
    title: &str,
    overrides: &[crate::settings::ProfileOverride],
) -> String {
    let exe_lc = exe.to_lowercase();
    let title_lc = title.to_lowercase();

    for ov in overrides {
        let pat = ov.pattern.trim().to_lowercase();
        if pat.is_empty() {
            // Пустой паттерн сматчил бы что угодно — пропускаем как невалидный.
            // Профиль не логируем как секрет — это публичная настройка, но
            // паттерн всё равно пуст, поэтому сообщение лаконично.
            log::warn!(
                "app_profile_overrides: правило с пустым pattern пропущено (профиль {:?})",
                ov.profile.trim()
            );
            continue;
        }
        let profile = ov.profile.trim();
        if !VALID_PROFILES.contains(&profile) {
            // Неизвестный профиль — правило игнорируем.
            log::warn!(
                "app_profile_overrides: неизвестный профиль {:?} в правиле (pattern {:?}) пропущен",
                profile,
                pat
            );
            continue;
        }
        if exe_lc.contains(&pat) || title_lc.contains(&pat) {
            // `code` — пользовательский псевдоним verbatim (rewrite выключен).
            let resolved = if profile == "code" {
                "verbatim"
            } else {
                profile
            };
            return resolved.to_string();
        }
    }

    classify(&exe_lc, &title_lc)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::category_for;
    use crate::settings::ProfileOverride;

    fn ov(pattern: &str, profile: &str) -> ProfileOverride {
        ProfileOverride {
            pattern: pattern.to_string(),
            profile: profile.to_string(),
        }
    }

    #[test]
    fn override_beats_builtin_profile() {
        let rules = vec![ov("telegram", "formal")];
        assert_eq!(category_for("telegram.exe", "Chat", &rules), "formal");
    }

    #[test]
    fn first_matching_override_wins() {
        let rules = vec![ov("chrome", "ai"), ov("chatgpt", "formal")];
        assert_eq!(category_for("chrome.exe", "ChatGPT", &rules), "ai");
    }

    #[test]
    fn empty_and_invalid_overrides_are_skipped() {
        let rules = vec![ov("  ", "formal"), ov("telegram", "unknown")];
        assert_eq!(category_for("telegram.exe", "Chat", &rules), "casual");
    }

    #[test]
    fn code_alias_resolves_to_verbatim() {
        let rules = vec![ov("cursor", "code")];
        assert_eq!(category_for("cursor.exe", "main.rs", &rules), "verbatim");
    }

    #[test]
    fn matching_is_case_insensitive_across_exe_and_title() {
        let rules = vec![ov("CoDeX", "ai"), ov("Quarterly", "doc")];
        assert_eq!(category_for("codex.exe", "Prompt", &rules), "ai");
        assert_eq!(category_for("word.exe", "Quarterly Plan", &rules), "doc");
    }

    #[test]
    fn codex_is_builtin_ai_context() {
        assert_eq!(super::classify("codex.exe", "New prompt"), "ai");
        assert_eq!(super::classify("chrome.exe", "codex - openai"), "ai");
    }

    fn ctx(exe: &str, title: &str, window_id: &str) -> super::AppContext {
        super::AppContext {
            exe: exe.to_string(),
            title: title.to_string(),
            window_id: window_id.to_string(),
            category: super::classify(exe, &title.to_lowercase()),
        }
    }

    #[test]
    fn target_fingerprint_uses_window_id_when_available() {
        let start = ctx("chrome.exe", "ChatGPT", "123").target_fingerprint();

        assert!(start.matches(&ctx("chrome.exe", "ChatGPT - updated", "123")));
        assert!(!start.matches(&ctx("chrome.exe", "ChatGPT", "456")));
        assert!(!start.matches(&ctx("msedge.exe", "ChatGPT", "123")));
    }

    #[test]
    fn target_fingerprint_falls_back_to_title_without_window_id() {
        let start = ctx("chrome.exe", "ChatGPT", "").target_fingerprint();

        assert!(start.matches(&ctx("chrome.exe", "ChatGPT", "")));
        assert!(!start.matches(&ctx("chrome.exe", "ChatGPT - updated", "")));
    }
}

// ===========================================================================
// Windows-реализация
// ===========================================================================

#[cfg(windows)]
mod platform {
    use super::{classify, AppContext};

    /// Право доступа к процессу: разрешает запрос ограниченной информации
    /// (в т.ч. полного пути к образу) без повышенных привилегий.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;

    // --- Сырые объявления функций WinAPI ----------------------------------
    //
    // HWND/HANDLE на уровне FFI представлены как `isize` (указатель-ширина).
    // На MSVC библиотеки user32/kernel32 линкуются автоматически, атрибут
    // #[link] здесь скорее декларативный.

    #[link(name = "user32")]
    extern "system" {
        /// Возвращает HWND окна переднего плана (0, если такого нет).
        fn GetForegroundWindow() -> isize;

        /// Записывает PID процесса-владельца окна в `lpdwProcessId`,
        /// возвращает идентификатор потока.
        fn GetWindowThreadProcessId(hwnd: isize, lpdwProcessId: *mut u32) -> u32;

        /// Копирует заголовок окна в буфер UTF-16, возвращает число
        /// скопированных символов (без завершающего нуля).
        fn GetWindowTextW(hwnd: isize, lpString: *mut u16, nMaxCount: i32) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        /// Открывает дескриптор процесса по PID (0/null при ошибке).
        fn OpenProcess(dwDesiredAccess: u32, bInheritHandle: i32, dwProcessId: u32) -> isize;

        /// Записывает полный путь к образу процесса в буфер UTF-16.
        /// `lpdwSize` на входе — размер буфера в символах, на выходе —
        /// число записанных символов. Возвращает ненулевое значение при успехе.
        fn QueryFullProcessImageNameW(
            hProcess: isize,
            dwFlags: u32,
            lpExeName: *mut u16,
            lpdwSize: *mut u32,
        ) -> i32;

        /// Закрывает ранее открытый дескриптор.
        fn CloseHandle(hObject: isize) -> i32;
    }

    /// Извлекает из полного пути имя файла (компонент после последнего
    /// `\\` или `/`) и приводит его к нижнему регистру.
    fn file_name_lowercase(full_path: &str) -> String {
        let name = full_path.rsplit(['\\', '/']).next().unwrap_or(full_path);
        name.to_lowercase()
    }

    /// Основная реализация определения контекста для Windows.
    pub fn detect() -> AppContext {
        // Все вызовы FFI сосредоточены в одном unsafe-блоке; логика внутри
        // максимально осторожна: при любом «нулевом» дескрипторе деградируем
        // до пустых строк.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd == 0 {
                return AppContext {
                    exe: String::new(),
                    title: String::new(),
                    window_id: String::new(),
                    category: "neutral".to_string(),
                };
            }
            let window_id = hwnd.to_string();

            // --- Заголовок окна -------------------------------------------
            let mut title_buf: [u16; 512] = [0u16; 512];
            let title_len = GetWindowTextW(hwnd, title_buf.as_mut_ptr(), title_buf.len() as i32);
            let title = if title_len > 0 {
                String::from_utf16_lossy(&title_buf[..title_len as usize])
            } else {
                String::new()
            };

            // --- PID процесса-владельца -----------------------------------
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, &mut pid as *mut u32);
            if pid == 0 {
                let category = classify("", &title.to_lowercase());
                return AppContext {
                    exe: String::new(),
                    title,
                    window_id,
                    category,
                };
            }

            // --- Полный путь к образу процесса ----------------------------
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            let mut exe = String::new();
            if process != 0 {
                let mut exe_buf: [u16; 1024] = [0u16; 1024];
                // На входе размер буфера в символах; функция перезапишет его
                // числом фактически записанных символов.
                let mut size: u32 = exe_buf.len() as u32;
                let ok = QueryFullProcessImageNameW(
                    process,
                    0,
                    exe_buf.as_mut_ptr(),
                    &mut size as *mut u32,
                );
                if ok != 0 && size > 0 {
                    // Защита от выхода за пределы буфера на всякий случай.
                    let len = (size as usize).min(exe_buf.len());
                    let full_path = String::from_utf16_lossy(&exe_buf[..len]);
                    exe = file_name_lowercase(&full_path);
                }
                // Дескриптор процесса обязательно закрываем.
                CloseHandle(process);
            }

            let category = classify(&exe, &title.to_lowercase());
            AppContext {
                exe,
                title,
                window_id,
                category,
            }
        }
    }
}

/// Определяет контекст активного приложения (Windows).
#[cfg(windows)]
pub fn detect() -> AppContext {
    platform::detect()
}

// ===========================================================================
// macOS-реализация (best-effort, без сторонних крейтов)
// ===========================================================================

/// Best-effort определение активного приложения на macOS через `osascript`.
///
/// Имя процесса переднего плана берётся у System Events
/// (`name of first application process whose frontmost is true`). Поскольку имя
/// процесса на macOS — это «отображаемое» имя приложения (напр. `"Safari"`,
/// `"Visual Studio Code"`), оно кладётся в поле `exe` в нижнем регистре —
/// встроенная таблица [`classify`] и так матчит по подстрокам/exe.
///
/// Заголовок окна best-effort пытаемся получить тем же `osascript`
/// (`name of front window`); если приложение не отдаёт имя окна или System
/// Events не имеет прав Accessibility — заголовок остаётся пустым (это
/// неблокирующая деградация, а не ошибка).
///
/// TODO: для надёжного заголовка окна и точного bundle id потребовался бы
/// доступ к AppKit/Accessibility (крейты `cocoa`/`objc` или `core-graphics`),
/// что выходит за рамки «без новых обязательных крейтов». Пока — best-effort
/// на `osascript`; Windows-сборки это не касается.
#[cfg(target_os = "macos")]
pub fn detect() -> AppContext {
    let snapshot = run_macos_context_script().unwrap_or_default();
    let app = snapshot.app.trim().to_string();
    let title = snapshot.title.trim().to_string();
    let exe = app.to_lowercase();
    let category = classify(&exe, &title.to_lowercase());
    let window_id = snapshot.window_id();
    AppContext {
        exe,
        title,
        window_id,
        category,
    }
}

#[derive(Default)]
struct MacContextSnapshot {
    app: String,
    pid: String,
    bundle: String,
    title: String,
    role: String,
    subrole: String,
    pos: String,
    size: String,
}

impl MacContextSnapshot {
    fn window_id(&self) -> String {
        let pid = self.pid.trim();
        if pid.is_empty() {
            return String::new();
        }
        let mut parts = vec![format!("pid={pid}")];
        if !self.bundle.trim().is_empty() {
            parts.push(format!("bundle={}", self.bundle.trim()));
        }
        if !self.role.trim().is_empty() {
            parts.push(format!("role={}", self.role.trim()));
        }
        if !self.subrole.trim().is_empty() {
            parts.push(format!("subrole={}", self.subrole.trim()));
        }
        if !self.pos.trim().is_empty() {
            parts.push(format!("pos={}", self.pos.trim()));
        }
        if !self.size.trim().is_empty() {
            parts.push(format!("size={}", self.size.trim()));
        }
        parts.join(";")
    }
}

/// Снимает контекст фронтального процесса одним AppleScript-вызовом.
///
/// Важно делать это атомарно: отдельные вызовы `osascript` иногда успевали увидеть
/// уже другое frontmost-окно (например, собственный overlay VoxFlow). PID + frame
/// дают macOS стабильный `window_id`, поэтому смена title во время распознавания
/// больше не отменяет финальную вставку.
#[cfg(target_os = "macos")]
fn run_macos_context_script() -> Option<MacContextSnapshot> {
    let script = r#"
tell application "System Events"
  set p to first application process whose frontmost is true
  set appName to name of p
  set appPid to unix id of p as text
  set bundleId to ""
  try
    set bundleId to bundle identifier of p
  end try
  set winTitle to ""
  set winRole to ""
  set winSubrole to ""
  set winPos to ""
  set winSize to ""
  try
    tell front window of p
      set winTitle to name as text
      set winRole to role as text
      set winSubrole to subrole as text
      set {x, y} to position
      set {w, h} to size
      set winPos to (x as text) & "," & (y as text)
      set winSize to (w as text) & "x" & (h as text)
    end tell
  end try
  return appName & linefeed & appPid & linefeed & bundleId & linefeed & winTitle & linefeed & winRole & linefeed & winSubrole & linefeed & winPos & linefeed & winSize
end tell
"#;
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_string();
    if s.trim().is_empty() {
        None
    } else {
        let parts: Vec<String> = s
            .split('\n')
            .map(|p| p.trim_end_matches('\r').to_string())
            .collect();
        Some(MacContextSnapshot {
            app: parts.first().cloned().unwrap_or_default(),
            pid: parts.get(1).cloned().unwrap_or_default(),
            bundle: parts.get(2).cloned().unwrap_or_default(),
            title: parts.get(3).cloned().unwrap_or_default(),
            role: parts.get(4).cloned().unwrap_or_default(),
            subrole: parts.get(5).cloned().unwrap_or_default(),
            pos: parts.get(6).cloned().unwrap_or_default(),
            size: parts.get(7).cloned().unwrap_or_default(),
        })
    }
}

// ===========================================================================
// Fallback для прочих платформ (не Windows и не macOS)
// ===========================================================================

/// Заглушка для платформ, где контекст определить нечем: возвращаем пустые
/// строки и нейтральную категорию.
#[cfg(not(any(windows, target_os = "macos")))]
pub fn detect() -> AppContext {
    AppContext {
        exe: String::new(),
        title: String::new(),
        window_id: String::new(),
        category: "neutral".into(),
    }
}
