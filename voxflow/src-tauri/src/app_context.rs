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
//! На macOS контекст снимается best-effort нативными read-only
//! Accessibility/CoreFoundation вызовами; на остальных не-Windows
//! платформах возвращается пустой `"neutral"`-контекст.

/// Контекст активного приложения.
///
/// * `exe` — имя исполняемого файла переднего окна в нижнем регистре
///   (например, `"chrome.exe"`); пустая строка, если определить не удалось.
/// * `title` — заголовок переднего окна; пустая строка при неудаче.
/// * `window_id` — стабильный идентификатор окна в рамках текущей сессии ОС
///   (на Windows это HWND); пустая строка, если платформа его не даёт.
/// * `category` — стилевая категория: `"verbatim" | "ai" | "formal" | "work"
///   | "casual" | "doc" | "neutral"`.
#[derive(Clone)]
pub struct AppContext {
    /// Имя exe-файла в нижнем регистре (только имя файла, без пути).
    pub exe: String,
    /// Заголовок переднего окна.
    pub title: String,
    /// Стабильный id foreground-окна, если платформа умеет его дать.
    pub window_id: String,
    /// Стилевая категория приложения.
    pub category: String,
    /// Роль сфокусированного элемента (best-effort Accessibility/UIA).
    pub field_role: String,
    /// Подроль сфокусированного элемента. Secure/password-поля не читаются.
    pub field_subrole: String,
    /// Стабильный best-effort id сфокусированного поля. Нужен, чтобы
    /// финальная вставка не ушла в другое поле того же окна.
    pub field_id: String,
    /// Локальный хвост текста активного поля, ограниченный и очищенный от
    /// переводов строк. Не логируется и не попадает во frontend API.
    pub field_text: String,
    /// Выделенный текст активного поля, если ОС отдаёт его без изменения UI.
    pub selected_text: String,
}

/// Отпечаток целевого окна для безопасной отложенной вставки.
///
/// Когда доступен `window_id`, он надёжнее заголовка: браузеры и Electron-приложения
/// часто меняют title между окончанием записи и финальной вставкой, из-за чего
/// строгая проверка `(exe,title)` ложно отменяла вставку.
#[derive(Clone, PartialEq, Eq)]
pub struct TargetFingerprint {
    exe: String,
    title: String,
    window_id: String,
    field_role: String,
    field_subrole: String,
    field_id: String,
    field_text: String,
    selected_text: String,
}

/// Context values can contain a fragment of the user's document. Keep Debug
/// useful for diagnostics while making accidental `{:?}` logging content-free.
impl std::fmt::Debug for AppContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppContext")
            .field("exe", &self.exe)
            .field("title_chars", &self.title.chars().count())
            .field("window_id", &self.window_id)
            .field("category", &self.category)
            .field("field_role", &self.field_role)
            .field("field_subrole", &self.field_subrole)
            .field("field_id_chars", &self.field_id.chars().count())
            .field("field_text_chars", &self.field_text.chars().count())
            .field("selected_text_chars", &self.selected_text.chars().count())
            .finish()
    }
}

impl std::fmt::Debug for TargetFingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TargetFingerprint")
            .field("exe", &self.exe)
            .field("title_chars", &self.title.chars().count())
            .field("window_id", &self.window_id)
            .field("field_role", &self.field_role)
            .field("field_subrole", &self.field_subrole)
            .field("field_id_chars", &self.field_id.chars().count())
            .field("field_text_chars", &self.field_text.chars().count())
            .field("selected_text_chars", &self.selected_text.chars().count())
            .finish()
    }
}

impl AppContext {
    pub fn target_fingerprint(&self) -> TargetFingerprint {
        TargetFingerprint {
            exe: self.exe.clone(),
            title: self.title.clone(),
            window_id: self.window_id.clone(),
            field_role: self.field_role.clone(),
            field_subrole: self.field_subrole.clone(),
            field_id: self.field_id.clone(),
            field_text: self.field_text.clone(),
            selected_text: self.selected_text.clone(),
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

    /// Rebuild the context captured at hotkey press without another platform
    /// query. The final pre-insert guard still performs a fresh detection; this
    /// cached value only removes redundant synchronous macOS Accessibility work
    /// before local ASR.
    pub fn captured_context(&self) -> AppContext {
        AppContext {
            exe: self.exe.clone(),
            title: self.title.clone(),
            window_id: self.window_id.clone(),
            category: classify(&self.exe, &self.title.to_ascii_lowercase()),
            field_role: self.field_role.clone(),
            field_subrole: self.field_subrole.clone(),
            field_id: self.field_id.clone(),
            field_text: self.field_text.clone(),
            selected_text: self.selected_text.clone(),
        }
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

    #[cfg(windows)]
    pub fn windows_hwnd(&self) -> Option<isize> {
        self.window_id
            .parse::<isize>()
            .ok()
            .filter(|hwnd| *hwnd != 0)
    }

    pub fn matches(&self, current: &AppContext) -> bool {
        let same_window = if !self.window_id.is_empty() && !current.window_id.is_empty() {
            self.exe == current.exe && self.window_id == current.window_id
        } else {
            self.exe == current.exe && self.title == current.title
        };
        same_window
            && (self.field_id.is_empty()
                || current.field_id.is_empty()
                || self.field_id == current.field_id)
    }
}

fn tail_chars(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let len = compact.chars().count();
    if len <= max_chars {
        return compact;
    }
    let tail = compact
        .chars()
        .rev()
        .take(max_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("...{}", tail.trim_start())
}

/// Privacy gate shared by the native detector and tests. AX uses the standard
/// `AXSecureTextField` subrole for password controls; custom controls sometimes
/// surface `password`/`secure` only in the role or role-description, so all
/// three pieces of metadata are checked before either value attribute is read.
fn is_sensitive_focused_field(role: &str, subrole: &str, role_description: &str) -> bool {
    [role, subrole, role_description].iter().any(|value| {
        let value = value.to_lowercase();
        value.contains("secure")
            || value.contains("password")
            || value.contains("passcode")
            || value.contains("credential")
            || value.contains("парол")
            || value.contains("защищ")
    })
}

/// Limit value reads to editable/textual controls. Besides reducing AX work,
/// this avoids treating button values, sliders and other UI metadata as local
/// document context.
fn is_textual_focused_field(role: &str, subrole: &str) -> bool {
    let role = role.to_ascii_lowercase();
    let subrole = subrole.to_ascii_lowercase();
    matches!(
        role.as_str(),
        "axtextfield" | "axtextarea" | "axcombobox" | "uiaedit" | "uiadocument"
    ) || subrole.contains("searchfield")
        || (role == "uiatext"
            && (subrole.contains("editable")
                || subrole.contains("textpattern")
                || subrole.contains("document")))
        || (role != "uiatext" && role.contains("text") && !role.contains("static"))
}

impl AppContext {
    /// Локальный контекст сфокусированного поля. Использует только
    /// `field_text`: выделение — это контекст замены/rewrite и не должно
    /// подменять соседний текст для обычной диктовки. Secure/password-поля
    /// никогда не возвращают текст.
    pub fn focused_text_tail(&self, max_chars: usize) -> Option<String> {
        focused_text_tail_from_parts(
            &self.field_role,
            &self.field_subrole,
            &self.field_text,
            max_chars,
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
}

fn focused_text_tail_from_parts(
    role: &str,
    subrole: &str,
    field_text: &str,
    max_chars: usize,
) -> Option<String> {
    if max_chars == 0 || is_sensitive_focused_field(role, subrole, "") {
        return None;
    }
    let value = field_text.trim();
    (!value.is_empty()).then(|| tail_chars(value, max_chars))
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
    // Windows shell surfaces can temporarily own foreground focus while a
    // tray/menu action opens VoxFlow. Match exact process names: substring
    // matching could reject an unrelated app. Explorer itself is intentionally
    // not listed because its address/search/rename fields are legitimate targets.
    const WINDOWS_EXES: &[&str] = &[
        "startmenuexperiencehost.exe",
        "shellexperiencehost.exe",
        "searchhost.exe",
        "searchui.exe",
        "textinputhost.exe",
        "lockapp.exe",
        "dwm.exe",
    ];
    let exe = exe.trim().to_ascii_lowercase();
    BUNDLES.iter().any(|b| window_id.contains(b))
        || EXES.iter().any(|name| exe == *name || exe.contains(name))
        || WINDOWS_EXES.iter().any(|name| exe == *name)
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
            field_role: String::new(),
            field_subrole: String::new(),
            field_id: String::new(),
            field_text: String::new(),
            selected_text: String::new(),
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

    #[test]
    fn target_fingerprint_rejects_another_field_in_the_same_window() {
        let mut start_context = ctx("chrome.exe", "Form", "window-1");
        start_context.field_id = "field-a".into();
        let start = start_context.target_fingerprint();

        let mut same_field = ctx("chrome.exe", "Form", "window-1");
        same_field.field_id = "field-a".into();
        assert!(start.matches(&same_field));

        let mut other_field = ctx("chrome.exe", "Form", "window-1");
        other_field.field_id = "field-b".into();
        assert!(!start.matches(&other_field));

        let unknown_field = ctx("chrome.exe", "Form", "window-1");
        assert!(
            start.matches(&unknown_field),
            "best-effort id may be unavailable"
        );
    }

    #[test]
    fn windows_shell_surfaces_are_not_dictation_targets() {
        for exe in [
            "StartMenuExperienceHost.exe",
            "ShellExperienceHost.exe",
            "SearchHost.exe",
            "TextInputHost.exe",
        ] {
            let context = ctx(exe, "Windows shell", "12345");
            assert!(context.is_transient_system_ui(), "{exe}");
            assert!(!context.is_usable_dictation_target(), "{exe}");
            assert!(
                context.target_fingerprint().is_transient_system_ui(),
                "{exe}"
            );
        }

        let explorer = ctx("explorer.exe", "Documents", "23456");
        assert!(explorer.is_usable_dictation_target());
    }

    #[test]
    fn focused_text_tail_uses_field_value_and_compacts_whitespace() {
        let mut context = ctx("pages", "Document", "window");
        context.field_role = "AXTextArea".into();
        context.field_text = "  whole\n\tfield value  ".into();
        context.selected_text = "  selected\n\twords  ".into();

        assert_eq!(
            context.focused_text_tail(1_600).as_deref(),
            Some("whole field value")
        );
    }

    #[test]
    fn focused_text_tail_is_unicode_bounded_and_zero_is_empty() {
        let mut context = ctx("pages", "Document", "window");
        context.field_role = "AXTextArea".into();
        context.field_text = "абвгд".into();

        assert_eq!(context.focused_text_tail(3).as_deref(), Some("...вгд"));
        assert_eq!(context.focused_text_tail(0), None);
    }

    #[test]
    fn secure_or_password_metadata_never_returns_focused_values() {
        for (role, subrole) in [
            ("AXTextField", "AXSecureTextField"),
            ("AXPasswordField", ""),
            ("AXTextField", "credential-input"),
        ] {
            let mut context = ctx("browser", "Login", "window");
            context.field_role = role.into();
            context.field_subrole = subrole.into();
            context.field_text = "do-not-read".into();
            context.selected_text = "also-secret".into();
            assert_eq!(context.focused_text_tail(1_600), None, "{role}/{subrole}");
        }
        assert!(super::is_sensitive_focused_field(
            "AXTextField",
            "",
            "Secure password text field"
        ));
        assert!(super::is_sensitive_focused_field(
            "AXTextField",
            "",
            "Защищённое поле пароля"
        ));
    }

    #[test]
    fn debug_output_redacts_title_and_focused_content() {
        let mut context = ctx("editor", "private document title", "window");
        context.field_role = "AXTextArea".into();
        context.field_text = "private field contents".into();
        context.selected_text = "private selection".into();

        let app_debug = format!("{context:?}");
        let fingerprint_debug = format!("{:?}", context.target_fingerprint());
        for output in [app_debug, fingerprint_debug] {
            assert!(!output.contains("private document title"));
            assert!(!output.contains("private field contents"));
            assert!(!output.contains("private selection"));
            assert!(output.contains("field_text_chars"));
        }
    }

    #[test]
    fn native_value_reads_are_limited_to_textual_roles() {
        assert!(super::is_textual_focused_field("AXTextField", ""));
        assert!(super::is_textual_focused_field("AXTextArea", ""));
        assert!(super::is_textual_focused_field(
            "AXTextField",
            "AXSearchField"
        ));
        assert!(super::is_textual_focused_field("UIAEdit", ""));
        assert!(super::is_textual_focused_field("UIADocument", ""));
        assert!(super::is_textual_focused_field("UIAText", "textpattern"));
        assert!(!super::is_textual_focused_field("UIAText", "Text"));
        assert!(!super::is_textual_focused_field("AXStaticText", ""));
        assert!(!super::is_textual_focused_field("AXButton", ""));
        assert!(!super::is_textual_focused_field("AXSlider", ""));
    }

    #[cfg(windows)]
    #[test]
    fn target_fingerprint_exposes_valid_windows_hwnd() {
        assert_eq!(
            ctx("notepad.exe", "Note", "12345")
                .target_fingerprint()
                .windows_hwnd(),
            Some(12345)
        );
        assert_eq!(
            ctx("notepad.exe", "Note", "0")
                .target_fingerprint()
                .windows_hwnd(),
            None
        );
        assert_eq!(
            ctx("notepad.exe", "Note", "invalid")
                .target_fingerprint()
                .windows_hwnd(),
            None
        );
    }
}

// ===========================================================================
// Windows-реализация
// ===========================================================================

#[cfg(windows)]
mod platform {
    use super::{classify, is_sensitive_focused_field, tail_chars, AppContext};
    use windows::core::Result as WinResult;
    use windows::Win32::Foundation::{RECT, RPC_E_CHANGED_MODE};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
        IUIAutomationTextRange, IUIAutomationValuePattern, TextPatternRangeEndpoint_End,
        TextPatternRangeEndpoint_Start, TextUnit_Character, UIA_CustomControlTypeId,
        UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_TextControlTypeId, UIA_TextPatternId,
        UIA_ValuePatternId, UIA_CONTROLTYPE_ID,
    };

    /// Право доступа к процессу: разрешает запрос ограниченной информации
    /// (в т.ч. полного пути к образу) без повышенных привилегий.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    /// Ни одна строка из UIA не попадает в контекст модели целиком.
    const FOCUSED_TEXT_LIMIT: usize = 1_600;
    const METADATA_LIMIT: usize = 192;
    const SELECTION_RANGE_LIMIT: i32 = 8;

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

    /// COM может уже быть инициализирован хозяином engine-thread в другой
    /// apartment-модели. RPC_E_CHANGED_MODE не означает, что COM недоступен:
    /// в этом случае не меняем apartment и не вызываем CoUninitialize.
    struct ComScope {
        must_uninitialize: bool,
    }

    impl ComScope {
        fn enter() -> Option<Self> {
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if result.is_ok() {
                Some(Self {
                    must_uninitialize: true,
                })
            } else if result == RPC_E_CHANGED_MODE {
                Some(Self {
                    must_uninitialize: false,
                })
            } else {
                None
            }
        }
    }

    impl Drop for ComScope {
        fn drop(&mut self) {
            if self.must_uninitialize {
                unsafe { CoUninitialize() };
            }
        }
    }

    #[derive(Default)]
    struct FocusedField {
        role: String,
        subrole: String,
        id: String,
        text: String,
        selected_text: String,
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    struct FieldRect {
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
    }

    impl FieldRect {
        fn from_uia(rect: RECT) -> Option<Self> {
            let value = Self {
                left: rect.left,
                top: rect.top,
                right: rect.right,
                bottom: rect.bottom,
            };
            (value.right > value.left && value.bottom > value.top).then_some(value)
        }
    }

    fn compact_metadata(value: &str) -> String {
        value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .take(METADATA_LIMIT)
            .collect()
    }

    fn canonical_role(control_type: UIA_CONTROLTYPE_ID) -> String {
        if control_type == UIA_EditControlTypeId {
            "UIAEdit".into()
        } else if control_type == UIA_DocumentControlTypeId {
            "UIADocument".into()
        } else if control_type == UIA_TextControlTypeId {
            "UIAText".into()
        } else {
            format!("UIAControl({})", control_type.0)
        }
    }

    fn subrole_metadata(
        localized_type: &str,
        class_name: &str,
        framework: &str,
        text_pattern: bool,
        password: bool,
    ) -> String {
        let mut parts = Vec::new();
        for value in [localized_type, class_name, framework] {
            let value = compact_metadata(value);
            if !value.is_empty() && !parts.iter().any(|known| known == &value) {
                parts.push(value);
            }
        }
        if text_pattern {
            // Shared textual-role gate accepts UIAText only when UIA confirms
            // that the focused element actually exposes TextPattern.
            parts.push("textpattern".into());
        }
        if password {
            parts.push("password".into());
        }
        compact_metadata(&parts.join(" | "))
    }

    fn field_identity(
        automation_id: &str,
        role: &str,
        class_name: &str,
        framework: &str,
        rect: Option<FieldRect>,
    ) -> String {
        let automation_id = compact_metadata(automation_id);
        let role = compact_metadata(role);
        let class_name = compact_metadata(class_name);
        let framework = compact_metadata(framework);
        if !automation_id.is_empty() {
            return compact_metadata(&format!("uia:id:{framework}:{class_name}:{automation_id}"));
        }
        let Some(rect) = rect else {
            // Role/class alone is not unique enough for two edit fields in one
            // window; an empty id deliberately disables the strict field guard.
            return String::new();
        };
        compact_metadata(&format!(
            "uia:frame:{role}:{class_name}:{},{},{},{}",
            rect.left, rect.top, rect.right, rect.bottom
        ))
    }

    fn bstr_metadata(result: WinResult<windows::core::BSTR>) -> String {
        result
            .map(|value| compact_metadata(&value.to_string()))
            .unwrap_or_default()
    }

    fn range_text(range: &IUIAutomationTextRange, limit: usize) -> String {
        if limit == 0 {
            return String::new();
        }
        unsafe { range.GetText(limit.min(i32::MAX as usize) as i32) }
            .map(|value| tail_chars(&value.to_string(), limit))
            .unwrap_or_default()
    }

    fn selected_text(pattern: &IUIAutomationTextPattern) -> String {
        let Ok(ranges) = (unsafe { pattern.GetSelection() }) else {
            return String::new();
        };
        let count = unsafe { ranges.Length() }
            .unwrap_or(0)
            .clamp(0, SELECTION_RANGE_LIMIT);
        let mut result = String::new();
        for index in 0..count {
            let remaining = FOCUSED_TEXT_LIMIT.saturating_sub(result.chars().count());
            if remaining == 0 {
                break;
            }
            let Ok(range) = (unsafe { ranges.GetElement(index) }) else {
                continue;
            };
            let part = range_text(&range, remaining);
            if part.is_empty() {
                continue;
            }
            if !result.is_empty() {
                result.push(' ');
            }
            result.push_str(&part);
        }
        tail_chars(&result, FOCUSED_TEXT_LIMIT)
    }

    /// Returns at most FOCUSED_TEXT_LIMIT characters immediately before the
    /// current selection/caret. If the provider exposes no selection range,
    /// falls back to the tail of DocumentRange without reading the full document.
    fn text_pattern_tail(pattern: &IUIAutomationTextPattern) -> String {
        if let Ok(ranges) = unsafe { pattern.GetSelection() } {
            if unsafe { ranges.Length() }.unwrap_or(0) > 0 {
                if let Ok(selection) = unsafe { ranges.GetElement(0) } {
                    if let (Ok(context), Ok(anchor)) =
                        (unsafe { selection.Clone() }, unsafe { selection.Clone() })
                    {
                        if unsafe {
                            context.MoveEndpointByRange(
                                TextPatternRangeEndpoint_End,
                                &anchor,
                                TextPatternRangeEndpoint_Start,
                            )
                        }
                        .is_ok()
                        {
                            let _ = unsafe {
                                context.MoveEndpointByUnit(
                                    TextPatternRangeEndpoint_Start,
                                    TextUnit_Character,
                                    -(FOCUSED_TEXT_LIMIT as i32),
                                )
                            };
                            let value = range_text(&context, FOCUSED_TEXT_LIMIT);
                            if !value.is_empty() {
                                return value;
                            }
                        }
                    }
                }
            }
        }

        let Ok(document) = (unsafe { pattern.DocumentRange() }) else {
            return String::new();
        };
        let Ok(context) = (unsafe { document.Clone() }) else {
            return String::new();
        };
        let Ok(end_anchor) = (unsafe { document.Clone() }) else {
            return String::new();
        };
        if unsafe {
            context.MoveEndpointByRange(
                TextPatternRangeEndpoint_Start,
                &end_anchor,
                TextPatternRangeEndpoint_End,
            )
        }
        .is_err()
        {
            return String::new();
        }
        let _ = unsafe {
            context.MoveEndpointByUnit(
                TextPatternRangeEndpoint_Start,
                TextUnit_Character,
                -(FOCUSED_TEXT_LIMIT as i32),
            )
        };
        range_text(&context, FOCUSED_TEXT_LIMIT)
    }

    fn focused_field(pid: u32) -> FocusedField {
        let Some(_com) = ComScope::enter() else {
            return FocusedField::default();
        };
        focused_field_inner(pid).unwrap_or_default()
    }

    fn focused_field_inner(pid: u32) -> WinResult<FocusedField> {
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
        let element: IUIAutomationElement = unsafe { automation.GetFocusedElement() }?;

        // The foreground HWND and UIA focus are sampled separately. Reject a
        // focus switch rather than reading another application's field.
        let element_pid = unsafe { element.CurrentProcessId() }?;
        if element_pid <= 0 || element_pid as u32 != pid {
            return Ok(FocusedField::default());
        }

        let control_type =
            unsafe { element.CurrentControlType() }.unwrap_or(UIA_CustomControlTypeId);
        let role = canonical_role(control_type);
        let class_name = bstr_metadata(unsafe { element.CurrentClassName() });
        let framework = bstr_metadata(unsafe { element.CurrentFrameworkId() });
        let localized_type = bstr_metadata(unsafe { element.CurrentLocalizedControlType() });
        let automation_id = bstr_metadata(unsafe { element.CurrentAutomationId() });
        let rect = unsafe { element.CurrentBoundingRectangle() }
            .ok()
            .and_then(FieldRect::from_uia);
        let password = unsafe { element.CurrentIsPassword() }
            .map(|value| value.as_bool())
            .unwrap_or(false);

        // Do not even request Value/TextPattern from protected controls. UIA
        // providers are allowed to reject value access for IsPassword=true,
        // and a buggy provider must not make us retain protected contents.
        if password || is_sensitive_focused_field(&role, &localized_type, &class_name) {
            let id = field_identity(&automation_id, &role, &class_name, &framework, rect);
            return Ok(FocusedField {
                role,
                subrole: subrole_metadata(&localized_type, &class_name, &framework, false, true),
                id,
                ..FocusedField::default()
            });
        }

        let text_pattern =
            unsafe { element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId) }
                .ok();
        let value_pattern =
            unsafe { element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) }
                .ok();
        let pattern_backed_text = text_pattern.is_some();
        let textual = control_type == UIA_EditControlTypeId
            || control_type == UIA_DocumentControlTypeId
            || (control_type == UIA_TextControlTypeId && pattern_backed_text);
        let subrole = subrole_metadata(
            &localized_type,
            &class_name,
            &framework,
            pattern_backed_text,
            password,
        );
        let id = field_identity(&automation_id, &role, &class_name, &framework, rect);

        if !textual {
            return Ok(FocusedField {
                role,
                subrole,
                id,
                ..FocusedField::default()
            });
        }

        let mut text = String::new();
        let mut selection = String::new();
        if let Some(pattern) = text_pattern.as_ref() {
            text = text_pattern_tail(pattern);
            selection = selected_text(pattern);
        }
        if text.is_empty() {
            if let Some(pattern) = value_pattern.as_ref() {
                text = unsafe { pattern.CurrentValue() }
                    .map(|value| tail_chars(&value.to_string(), FOCUSED_TEXT_LIMIT))
                    .unwrap_or_default();
            }
        }

        Ok(FocusedField {
            role,
            subrole,
            id,
            text,
            selected_text: selection,
        })
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
                    field_role: String::new(),
                    field_subrole: String::new(),
                    field_id: String::new(),
                    field_text: String::new(),
                    selected_text: String::new(),
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
                    field_role: String::new(),
                    field_subrole: String::new(),
                    field_id: String::new(),
                    field_text: String::new(),
                    selected_text: String::new(),
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
            let field = focused_field(pid);
            AppContext {
                exe,
                title,
                window_id,
                category,
                field_role: field.role,
                field_subrole: field.subrole,
                field_id: field.id,
                field_text: field.text,
                selected_text: field.selected_text,
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn canonical_text_roles_are_stable() {
            assert_eq!(canonical_role(UIA_EditControlTypeId), "UIAEdit");
            assert_eq!(canonical_role(UIA_DocumentControlTypeId), "UIADocument");
            assert_eq!(canonical_role(UIA_TextControlTypeId), "UIAText");
            assert_eq!(
                canonical_role(UIA_CONTROLTYPE_ID(50_999)),
                "UIAControl(50999)"
            );
        }

        #[test]
        fn automation_id_is_preferred_and_metadata_is_bounded() {
            let id = field_identity(
                " editor  field ",
                "UIAEdit",
                "RichEditD2DPT",
                "Win32",
                Some(FieldRect {
                    left: 1,
                    top: 2,
                    right: 300,
                    bottom: 40,
                }),
            );
            assert_eq!(id, "uia:id:Win32:RichEditD2DPT:editor field");
            assert!(id.chars().count() <= METADATA_LIMIT);
        }

        #[test]
        fn role_and_frame_are_safe_identity_fallback() {
            let rect = FieldRect {
                left: 10,
                top: 20,
                right: 410,
                bottom: 120,
            };
            assert_eq!(
                field_identity(
                    "",
                    "UIADocument",
                    "Chrome_RenderWidgetHostHWND",
                    "Chrome",
                    Some(rect)
                ),
                "uia:frame:UIADocument:Chrome_RenderWidgetHostHWND:10,20,410,120"
            );
            assert!(field_identity("", "UIAEdit", "Edit", "Win32", None).is_empty());
        }

        #[test]
        fn invalid_or_zero_sized_frames_are_rejected() {
            assert_eq!(
                FieldRect::from_uia(RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 30,
                }),
                None
            );
            assert_eq!(
                FieldRect::from_uia(RECT {
                    left: 50,
                    top: 50,
                    right: 20,
                    bottom: 80,
                }),
                None
            );
        }

        #[test]
        fn password_and_textpattern_markers_are_explicit() {
            let metadata = subrole_metadata("edit", "PasswordBox", "WPF", true, true);
            assert!(metadata.contains("textpattern"));
            assert!(metadata.contains("password"));
            assert!(is_sensitive_focused_field(
                "UIAEdit",
                &metadata,
                "PasswordBox"
            ));
        }

        #[test]
        fn metadata_compaction_is_unicode_safe() {
            let source = format!("  {}  suffix  ", "я".repeat(METADATA_LIMIT + 20));
            let compact = compact_metadata(&source);
            assert_eq!(compact.chars().count(), METADATA_LIMIT);
            assert!(!compact.contains('\n'));
        }
    }
}

/// Определяет контекст активного приложения (Windows).
#[cfg(windows)]
pub fn detect() -> AppContext {
    platform::detect()
}

// ===========================================================================
// macOS-реализация (native AX/CoreFoundation, read-only)
// ===========================================================================

#[cfg(target_os = "macos")]
mod macos {
    use super::{
        classify, is_sensitive_focused_field, is_textual_focused_field, tail_chars, AppContext,
    };
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::ptr;

    type AXError = i32;
    type AXUIElementRef = *const c_void;
    type AXValueRef = *const c_void;
    type CFAllocatorRef = *const c_void;
    type CFBundleRef = *const c_void;
    type CFIndex = isize;
    type CFStringRef = *const c_void;
    type CFTypeID = usize;
    type CFTypeRef = *const c_void;
    type CFURLRef = *const c_void;

    const AX_SUCCESS: AXError = 0;
    const AX_VALUE_CGPOINT: u32 = 1;
    const AX_VALUE_CGSIZE: u32 = 2;
    const AX_TIMEOUT_SECONDS: f32 = 0.08;
    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const FOCUSED_TEXT_LIMIT: usize = 1_600;
    const METADATA_UTF16_LIMIT: usize = 4_096;
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4_096;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CFRange {
        location: CFIndex,
        length: CFIndex,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
        fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
        fn AXUIElementGetTypeID() -> CFTypeID;
        fn AXUIElementSetMessagingTimeout(
            element: AXUIElementRef,
            timeout_in_seconds: f32,
        ) -> AXError;
        fn AXValueGetTypeID() -> CFTypeID;
        fn AXValueGetValue(value: AXValueRef, value_type: u32, value_ptr: *mut c_void) -> u8;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFRelease(value: CFTypeRef);
        fn CFGetTypeID(value: CFTypeRef) -> CFTypeID;
        fn CFStringGetTypeID() -> CFTypeID;
        fn CFStringCreateWithCString(
            allocator: CFAllocatorRef,
            c_str: *const c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFStringGetLength(string: CFStringRef) -> CFIndex;
        fn CFStringGetCharacterAtIndex(string: CFStringRef, index: CFIndex) -> u16;
        fn CFStringGetBytes(
            string: CFStringRef,
            range: CFRange,
            encoding: u32,
            loss_byte: u8,
            is_external_representation: u8,
            buffer: *mut u8,
            max_buffer_length: CFIndex,
            used_buffer_length: *mut CFIndex,
        ) -> CFIndex;
        fn CFURLCreateFromFileSystemRepresentation(
            allocator: CFAllocatorRef,
            buffer: *const u8,
            buffer_length: CFIndex,
            is_directory: u8,
        ) -> CFURLRef;
        fn CFBundleCreate(allocator: CFAllocatorRef, bundle_url: CFURLRef) -> CFBundleRef;
        fn CFBundleGetIdentifier(bundle: CFBundleRef) -> CFStringRef;
    }

    #[link(name = "proc")]
    extern "C" {
        fn proc_pidpath(pid: i32, buffer: *mut c_void, buffer_size: u32) -> i32;
    }

    struct OwnedCf(CFTypeRef);

    impl OwnedCf {
        fn new(value: CFTypeRef) -> Option<Self> {
            (!value.is_null()).then_some(Self(value))
        }

        fn as_ptr(&self) -> CFTypeRef {
            self.0
        }

        fn is_type(&self, expected: CFTypeID) -> bool {
            unsafe { CFGetTypeID(self.0) == expected }
        }
    }

    impl Drop for OwnedCf {
        fn drop(&mut self) {
            unsafe { CFRelease(self.0) };
        }
    }

    #[derive(Default)]
    struct MacContextSnapshot {
        app: String,
        process_name: String,
        pid: i32,
        bundle: String,
        title: String,
        position: Option<CGPoint>,
        size: Option<CGSize>,
        field_role: String,
        field_subrole: String,
        field_id: String,
        field_text: String,
        selected_text: String,
    }

    impl MacContextSnapshot {
        fn window_id(&self) -> String {
            if self.pid <= 0 {
                return String::new();
            }
            let mut parts = vec![format!("pid={}", self.pid)];
            if !self.bundle.is_empty() {
                parts.push(format!("bundle={}", self.bundle));
            }
            if let Some(point) = self.position.filter(|point| finite_point(*point)) {
                parts.push(format!("pos={:.0},{:.0}", point.x, point.y));
            }
            if let Some(size) = self.size.filter(|size| finite_size(*size)) {
                parts.push(format!("size={:.0}x{:.0}", size.width, size.height));
            }
            parts.join(";")
        }
    }

    fn finite_point(point: CGPoint) -> bool {
        point.x.is_finite()
            && point.y.is_finite()
            && point.x.abs() < 10_000_000.0
            && point.y.abs() < 10_000_000.0
    }

    fn finite_size(size: CGSize) -> bool {
        size.width.is_finite()
            && size.height.is_finite()
            && size.width >= 0.0
            && size.height >= 0.0
            && size.width < 10_000_000.0
            && size.height < 10_000_000.0
    }

    fn field_identity(
        identifier: &str,
        role: &str,
        subrole: &str,
        position: Option<CGPoint>,
        size: Option<CGSize>,
    ) -> String {
        let role = identity_component(role, 80);
        let subrole = identity_component(subrole, 80);
        let identifier = identity_component(identifier, 256);
        if !identifier.is_empty() {
            return format!("id={identifier};role={role};subrole={subrole}");
        }
        match (
            position.filter(|point| finite_point(*point)),
            size.filter(|size| finite_size(*size)),
        ) {
            (Some(point), Some(size)) => format!(
                "role={role};subrole={subrole};pos={:.0},{:.0};size={:.0}x{:.0}",
                point.x, point.y, size.width, size.height
            ),
            _ => String::new(),
        }
    }

    fn identity_component(value: &str, max_chars: usize) -> String {
        value
            .trim()
            .chars()
            .take(max_chars)
            .map(|character| match character {
                ';' | '=' | '\r' | '\n' | '\t' => '_',
                character if character.is_control() => '_',
                character => character,
            })
            .collect()
    }

    unsafe fn copy_attribute(element: AXUIElementRef, name: &str) -> Option<OwnedCf> {
        let name = CString::new(name).ok()?;
        let attribute = OwnedCf::new(CFStringCreateWithCString(
            ptr::null(),
            name.as_ptr(),
            CF_STRING_ENCODING_UTF8,
        ))?;
        let mut value: CFTypeRef = ptr::null();
        if AXUIElementCopyAttributeValue(element, attribute.as_ptr() as CFStringRef, &mut value)
            != AX_SUCCESS
        {
            return None;
        }
        OwnedCf::new(value)
    }

    unsafe fn copy_element(element: AXUIElementRef, attribute: &str) -> Option<OwnedCf> {
        let value = copy_attribute(element, attribute)?;
        value.is_type(AXUIElementGetTypeID()).then_some(value)
    }

    unsafe fn copy_string(element: AXUIElementRef, attribute: &str) -> Option<String> {
        let value = copy_attribute(element, attribute)?;
        cf_string_slice(value.as_ptr(), METADATA_UTF16_LIMIT, false).map(|(text, _)| text)
    }

    unsafe fn copy_text_tail(element: AXUIElementRef, attribute: &str) -> Option<String> {
        let value = copy_attribute(element, attribute)?;
        let (raw, source_truncated) = cf_string_slice(value.as_ptr(), FOCUSED_TEXT_LIMIT, true)?;
        let compact = tail_chars(&raw, FOCUSED_TEXT_LIMIT);
        if compact.is_empty() {
            return None;
        }
        if source_truncated && !compact.starts_with("...") {
            Some(format!("...{}", compact.trim_start()))
        } else {
            Some(compact)
        }
    }

    /// Convert at most `max_utf16_units` from a CFString. Focused text uses a
    /// suffix range, so a multi-megabyte editor value is never copied wholesale
    /// into Rust memory merely to retain its tail.
    unsafe fn cf_string_slice(
        value: CFTypeRef,
        max_utf16_units: usize,
        tail: bool,
    ) -> Option<(String, bool)> {
        if value.is_null() || CFGetTypeID(value) != CFStringGetTypeID() {
            return None;
        }
        let total = CFStringGetLength(value as CFStringRef).max(0) as usize;
        if total == 0 || max_utf16_units == 0 {
            return Some((String::new(), false));
        }
        let mut units = total.min(max_utf16_units);
        let mut location = if tail { total - units } else { 0 };
        // A suffix boundary may land between an emoji's UTF-16 surrogate pair.
        // Include the preceding high surrogate, then apply the scalar-char tail
        // limit in Rust. This reads at most one extra UTF-16 code unit.
        if tail
            && location > 0
            && matches!(
                CFStringGetCharacterAtIndex(value as CFStringRef, location as CFIndex),
                0xDC00..=0xDFFF
            )
        {
            location -= 1;
            units += 1;
        }
        let mut bytes = vec![0u8; units.saturating_mul(4).saturating_add(4)];
        let mut used: CFIndex = 0;
        let converted = CFStringGetBytes(
            value as CFStringRef,
            CFRange {
                location: location as CFIndex,
                length: units as CFIndex,
            },
            CF_STRING_ENCODING_UTF8,
            0,
            0,
            bytes.as_mut_ptr(),
            bytes.len() as CFIndex,
            &mut used,
        );
        if converted <= 0 || used < 0 {
            return None;
        }
        bytes.truncate((used as usize).min(bytes.len()));
        Some((
            String::from_utf8_lossy(&bytes).into_owned(),
            tail && total > units,
        ))
    }

    unsafe fn copy_ax_point(element: AXUIElementRef, attribute: &str) -> Option<CGPoint> {
        let value = copy_attribute(element, attribute)?;
        if !value.is_type(AXValueGetTypeID()) {
            return None;
        }
        let mut point = CGPoint::default();
        (AXValueGetValue(
            value.as_ptr() as AXValueRef,
            AX_VALUE_CGPOINT,
            &mut point as *mut CGPoint as *mut c_void,
        ) != 0)
            .then_some(point)
    }

    unsafe fn copy_ax_size(element: AXUIElementRef, attribute: &str) -> Option<CGSize> {
        let value = copy_attribute(element, attribute)?;
        if !value.is_type(AXValueGetTypeID()) {
            return None;
        }
        let mut size = CGSize::default();
        (AXValueGetValue(
            value.as_ptr() as AXValueRef,
            AX_VALUE_CGSIZE,
            &mut size as *mut CGSize as *mut c_void,
        ) != 0)
            .then_some(size)
    }

    fn process_path(pid: i32) -> Option<PathBuf> {
        let mut buffer = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
        let result =
            unsafe { proc_pidpath(pid, buffer.as_mut_ptr() as *mut c_void, buffer.len() as u32) };
        if result <= 0 {
            return None;
        }
        let path = unsafe { CStr::from_ptr(buffer.as_ptr() as *const c_char) };
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(path.to_bytes())))
    }

    fn enclosing_app_bundle(executable: &Path) -> Option<&Path> {
        executable.ancestors().find(|ancestor| {
            ancestor
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        })
    }

    fn bundle_identifier(bundle_path: &Path) -> Option<String> {
        let bytes = bundle_path.as_os_str().as_bytes();
        unsafe {
            let url = OwnedCf::new(CFURLCreateFromFileSystemRepresentation(
                ptr::null(),
                bytes.as_ptr(),
                bytes.len() as CFIndex,
                1,
            ))?;
            let bundle = OwnedCf::new(CFBundleCreate(ptr::null(), url.as_ptr() as CFURLRef))?;
            let identifier = CFBundleGetIdentifier(bundle.as_ptr() as CFBundleRef);
            cf_string_slice(identifier, 512, false).map(|(identifier, _)| identifier)
        }
    }

    fn native_snapshot() -> Option<MacContextSnapshot> {
        unsafe {
            let system = OwnedCf::new(AXUIElementCreateSystemWide())?;
            let app = copy_element(system.as_ptr() as AXUIElementRef, "AXFocusedApplication")?;
            let app_ref = app.as_ptr() as AXUIElementRef;
            let _ = AXUIElementSetMessagingTimeout(app_ref, AX_TIMEOUT_SECONDS);

            let mut pid = 0i32;
            if AXUIElementGetPid(app_ref, &mut pid) != AX_SUCCESS || pid <= 0 {
                return None;
            }
            let executable = process_path(pid);
            let process_name = executable
                .as_deref()
                .and_then(Path::file_name)
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let bundle = executable
                .as_deref()
                .and_then(enclosing_app_bundle)
                .and_then(bundle_identifier)
                .unwrap_or_default();

            let mut snapshot = MacContextSnapshot {
                app: copy_string(app_ref, "AXTitle").unwrap_or_default(),
                process_name,
                pid,
                bundle,
                ..MacContextSnapshot::default()
            };

            if let Some(window) = copy_element(app_ref, "AXFocusedWindow") {
                let window_ref = window.as_ptr() as AXUIElementRef;
                let _ = AXUIElementSetMessagingTimeout(window_ref, AX_TIMEOUT_SECONDS);
                snapshot.title = copy_string(window_ref, "AXTitle").unwrap_or_default();
                snapshot.position = copy_ax_point(window_ref, "AXPosition");
                snapshot.size = copy_ax_size(window_ref, "AXSize");
            }

            if let Some(field) = copy_element(app_ref, "AXFocusedUIElement") {
                let field_ref = field.as_ptr() as AXUIElementRef;
                let _ = AXUIElementSetMessagingTimeout(field_ref, AX_TIMEOUT_SECONDS);
                snapshot.field_role = copy_string(field_ref, "AXRole").unwrap_or_default();
                snapshot.field_subrole = copy_string(field_ref, "AXSubrole").unwrap_or_default();
                let role_description =
                    copy_string(field_ref, "AXRoleDescription").unwrap_or_default();
                let identifier = copy_string(field_ref, "AXIdentifier")
                    .filter(|identifier| !identifier.trim().is_empty())
                    .or_else(|| copy_string(field_ref, "AXDOMIdentifier"))
                    .unwrap_or_default();
                let (field_position, field_size) = if identifier.trim().is_empty() {
                    (
                        copy_ax_point(field_ref, "AXPosition"),
                        copy_ax_size(field_ref, "AXSize"),
                    )
                } else {
                    (None, None)
                };
                snapshot.field_id = field_identity(
                    &identifier,
                    &snapshot.field_role,
                    &snapshot.field_subrole,
                    field_position,
                    field_size,
                );

                if !is_sensitive_focused_field(
                    &snapshot.field_role,
                    &snapshot.field_subrole,
                    &role_description,
                ) && is_textual_focused_field(&snapshot.field_role, &snapshot.field_subrole)
                {
                    // Privacy invariant: neither value attribute is even queried
                    // until the secure/password gate above has passed.
                    snapshot.field_text = copy_text_tail(field_ref, "AXValue").unwrap_or_default();
                    snapshot.selected_text =
                        copy_text_tail(field_ref, "AXSelectedText").unwrap_or_default();
                }
            }

            Some(snapshot)
        }
    }

    pub fn detect() -> AppContext {
        let snapshot = native_snapshot().unwrap_or_default();
        let app = if snapshot.app.trim().is_empty() {
            snapshot.process_name.trim()
        } else {
            snapshot.app.trim()
        };
        let exe = app.to_lowercase();
        let title = snapshot.title.trim().to_string();
        AppContext {
            category: classify(&exe, &title.to_lowercase()),
            exe,
            title,
            window_id: snapshot.window_id(),
            field_role: snapshot.field_role,
            field_subrole: snapshot.field_subrole,
            field_id: snapshot.field_id,
            field_text: snapshot.field_text,
            selected_text: snapshot.selected_text,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn cf_string_tail_converts_only_requested_suffix() {
            let source = format!("{}{}", "A".repeat(1_000), "B".repeat(1_700));
            let source = CString::new(source).unwrap();
            let value = unsafe {
                OwnedCf::new(CFStringCreateWithCString(
                    ptr::null(),
                    source.as_ptr(),
                    CF_STRING_ENCODING_UTF8,
                ))
                .unwrap()
            };
            let (tail, truncated) =
                unsafe { cf_string_slice(value.as_ptr(), FOCUSED_TEXT_LIMIT, true).unwrap() };

            assert!(truncated);
            assert_eq!(tail.chars().count(), FOCUSED_TEXT_LIMIT);
            assert!(tail.chars().all(|character| character == 'B'));
        }

        #[test]
        fn cf_string_tail_does_not_split_surrogate_pairs() {
            let source = format!("{}{}Z", "A".repeat(1_000), "🙂".repeat(1_000));
            let source = CString::new(source).unwrap();
            let value = unsafe {
                OwnedCf::new(CFStringCreateWithCString(
                    ptr::null(),
                    source.as_ptr(),
                    CF_STRING_ENCODING_UTF8,
                ))
                .unwrap()
            };
            let (tail, truncated) =
                unsafe { cf_string_slice(value.as_ptr(), FOCUSED_TEXT_LIMIT, true).unwrap() };

            assert!(truncated);
            assert!(tail.ends_with('Z'));
            assert!(!tail.contains('�'));
            assert!(tail[..tail.len() - 1]
                .chars()
                .all(|character| character == '🙂'));
        }

        #[test]
        fn window_identity_is_pid_bundle_and_frame() {
            let snapshot = MacContextSnapshot {
                pid: 42,
                bundle: "com.example.Editor".into(),
                position: Some(CGPoint { x: 10.2, y: 20.8 }),
                size: Some(CGSize {
                    width: 800.4,
                    height: 600.6,
                }),
                ..MacContextSnapshot::default()
            };
            assert_eq!(
                snapshot.window_id(),
                "pid=42;bundle=com.example.Editor;pos=10,21;size=800x601"
            );
        }

        #[test]
        fn field_identity_prefers_identifier_then_falls_back_to_role_and_frame() {
            assert_eq!(
                field_identity(
                    "compose;body=main",
                    "AXTextArea",
                    "",
                    Some(CGPoint { x: 1.0, y: 2.0 }),
                    Some(CGSize {
                        width: 3.0,
                        height: 4.0,
                    }),
                ),
                "id=compose_body_main;role=AXTextArea;subrole="
            );
            assert_eq!(
                field_identity(
                    "",
                    "AXTextField",
                    "AXSearchField",
                    Some(CGPoint { x: 10.4, y: 20.6 }),
                    Some(CGSize {
                        width: 300.2,
                        height: 40.1,
                    }),
                ),
                "role=AXTextField;subrole=AXSearchField;pos=10,21;size=300x40"
            );
            assert!(field_identity("", "AXTextField", "", None, None).is_empty());
        }

        #[test]
        fn native_detection_smoke_is_bounded_and_text_is_capped() {
            let started = std::time::Instant::now();
            let context = detect();
            let elapsed = started.elapsed();
            eprintln!(
                "native AppContext detect: {} us (populated={})",
                elapsed.as_micros(),
                !context.window_id.is_empty()
            );

            assert!(elapsed < std::time::Duration::from_secs(2));
            assert!(context.field_text.chars().count() <= FOCUSED_TEXT_LIMIT + 3);
            assert!(context.selected_text.chars().count() <= FOCUSED_TEXT_LIMIT + 3);
        }
    }
}

#[cfg(target_os = "macos")]
pub fn detect() -> AppContext {
    macos::detect()
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
        field_role: String::new(),
        field_subrole: String::new(),
        field_id: String::new(),
        field_text: String::new(),
        selected_text: String::new(),
    }
}
