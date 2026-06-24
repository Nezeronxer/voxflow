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
/// * `category` — стилевая категория: `"verbatim" | "ai" | "formal" | "work"
///   | "casual" | "doc" | "neutral"`.
pub struct AppContext {
    /// Имя exe-файла в нижнем регистре (только имя файла, без пути).
    pub exe: String,
    /// Заголовок переднего окна.
    pub title: String,
    /// Стилевая категория приложения.
    pub category: String,
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
                    category: "neutral".to_string(),
                };
            }

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
    // Достаём имя фронтального приложения.
    let app = run_osascript(
        "tell application \"System Events\" to get name of first application process whose frontmost is true",
    )
    .unwrap_or_default();

    // Заголовок окна — отдельным best-effort вызовом; ошибки/пустоту глотаем.
    let title = if app.is_empty() {
        String::new()
    } else {
        run_osascript(&format!(
            "tell application \"System Events\" to tell process \"{}\" to get name of front window",
            app.replace('"', "")
        ))
        .unwrap_or_default()
    };

    let exe = app.to_lowercase();
    let category = classify(&exe, &title.to_lowercase());
    AppContext {
        exe,
        title,
        category,
    }
}

/// Выполняет одну строку AppleScript через `osascript -e` и возвращает
/// обрезанный stdout. Любая ошибка запуска/ненулевой код → `None`.
#[cfg(target_os = "macos")]
fn run_osascript(script: &str) -> Option<String> {
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
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
        category: "neutral".into(),
    }
}
