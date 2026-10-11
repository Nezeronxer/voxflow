//! Linux: определение сессии (X11 / Wayland), внешние утилиты для вставки текста
//! и подсказки по пакетам дистрибутива.
//!
//! Модуль не использует ничего, кроме std, поэтому собирается и на других ОС
//! (только ради тестов и проверки типов); в рабочем коде зовётся под Linux.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use anyhow::{anyhow, Result};
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Session {
    X11,
    Wayland,
    Unknown,
}

fn non_empty(v: Option<&str>) -> bool {
    v.is_some_and(|s| !s.trim().is_empty())
}

fn session_from(
    xdg: Option<&str>,
    wayland_display: Option<&str>,
    display: Option<&str>,
) -> Session {
    match xdg.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("wayland") => Session::Wayland,
        Some("x11") => Session::X11,
        // tty / unspecified / ssh: смотрим на сами сокеты дисплея.
        _ if non_empty(wayland_display) => Session::Wayland,
        _ if non_empty(display) => Session::X11,
        _ => Session::Unknown,
    }
}

pub fn session() -> Session {
    let get = |k: &str| std::env::var(k).ok();
    session_from(
        get("XDG_SESSION_TYPE").as_deref(),
        get("WAYLAND_DISPLAY").as_deref(),
        get("DISPLAY").as_deref(),
    )
}

// ───────────────────────────── пакеты дистрибутива ─────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Apt,
    Dnf,
    Pacman,
    Zypper,
    Unknown,
}

fn package_manager_from(os_release: &str) -> PackageManager {
    let mut ids: Vec<String> = Vec::new();
    for line in os_release.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key == "ID" || key == "ID_LIKE" {
            let value = value.trim().trim_matches('"').to_ascii_lowercase();
            ids.extend(value.split_whitespace().map(str::to_string));
        }
    }
    let has = |names: &[&str]| ids.iter().any(|id| names.contains(&id.as_str()));
    if has(&["debian", "ubuntu", "linuxmint", "pop"]) {
        PackageManager::Apt
    } else if has(&["fedora", "rhel", "centos", "rocky", "almalinux"]) {
        PackageManager::Dnf
    } else if has(&["arch", "manjaro", "endeavouros"]) {
        PackageManager::Pacman
    } else if has(&[
        "opensuse",
        "suse",
        "opensuse-leap",
        "opensuse-tumbleweed",
        "sles",
    ]) {
        PackageManager::Zypper
    } else {
        PackageManager::Unknown
    }
}

/// Утилита и её имя пакета в разных дистрибутивах.
struct Tool {
    bin: &'static str,
    apt: &'static str,
    dnf: &'static str,
    pacman: &'static str,
    zypper: &'static str,
}

const WTYPE: Tool = Tool {
    bin: "wtype",
    apt: "wtype",
    dnf: "wtype",
    pacman: "wtype",
    zypper: "wtype",
};
const YDOTOOL: Tool = Tool {
    bin: "ydotool",
    apt: "ydotool",
    dnf: "ydotool",
    pacman: "ydotool",
    zypper: "ydotool",
};
const PACTL: Tool = Tool {
    bin: "pactl",
    apt: "pulseaudio-utils",
    dnf: "pulseaudio-utils",
    pacman: "libpulse",
    zypper: "pulseaudio-utils",
};
const XPROP: Tool = Tool {
    bin: "xprop",
    apt: "x11-utils",
    dnf: "xorg-x11-utils",
    pacman: "xorg-xprop",
    zypper: "xprop",
};

fn install_command(tool: &Tool, pm: PackageManager) -> String {
    match pm {
        PackageManager::Apt => format!("sudo apt install {}", tool.apt),
        PackageManager::Dnf => format!("sudo dnf install {}", tool.dnf),
        PackageManager::Pacman => format!("sudo pacman -S {}", tool.pacman),
        PackageManager::Zypper => format!("sudo zypper install {}", tool.zypper),
        PackageManager::Unknown => format!("установите пакет с утилитой {}", tool.bin),
    }
}

fn install_hint(tool: &Tool) -> String {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    install_command(tool, package_manager_from(&os_release))
}

fn find_in_path(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(bin))
            .find(|candidate| candidate.is_file())
    })
}

fn run(bin: &str, args: &[String]) -> Result<()> {
    let out = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| anyhow!("{bin}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "{bin} завершился с {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn s(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_string()).collect()
}

/// Пробует по очереди установленные утилиты; первая успешная выигрывает.
/// Если не установлена ни одна — ошибка с названием пакета для дистрибутива.
fn first_ok(attempts: Vec<(&'static Tool, Vec<String>)>) -> Result<()> {
    let mut last_error: Option<anyhow::Error> = None;
    for (tool, args) in &attempts {
        if find_in_path(tool.bin).is_none() {
            continue;
        }
        match run(tool.bin, args) {
            Ok(()) => return Ok(()),
            Err(e) => last_error = Some(e),
        }
    }
    if let Some(e) = last_error {
        return Err(e.context(
            "не удалось отправить ввод в сессию Wayland (ydotool требует запущенный ydotoold \
             и доступ к /dev/uinput; wtype не работает в GNOME и KDE)",
        ));
    }
    let hints: Vec<String> = attempts
        .iter()
        .map(|(tool, _)| install_hint(tool))
        .collect();
    Err(anyhow!(
        "в сессии Wayland для вставки текста нужна утилита wtype (Sway, Hyprland) или ydotool \
         (GNOME, KDE). Установите одну из них: {}",
        hints.join(" / ")
    ))
}

// ───────────────────────────── Wayland: ввод ─────────────────────────────

// Коды linux/input-event-codes.h.
const KEY_LEFTCTRL: &str = "29";
const KEY_LEFTSHIFT: &str = "42";
const KEY_BACKSPACE: &str = "14";

fn evdev_letter(letter: char) -> Option<&'static str> {
    match letter {
        'v' => Some("47"),
        'c' => Some("46"),
        _ => None,
    }
}

fn wtype_chord_args(letter: char, shift: bool) -> Vec<String> {
    let mut a = s(&["-M", "ctrl"]);
    if shift {
        a.extend(s(&["-M", "shift"]));
    }
    a.extend(s(&["-k", &letter.to_string()]));
    if shift {
        a.extend(s(&["-m", "shift"]));
    }
    a.extend(s(&["-m", "ctrl"]));
    a
}

fn ydotool_chord_args(code: &str, shift: bool) -> Vec<String> {
    let mut a = s(&["key", &format!("{KEY_LEFTCTRL}:1")]);
    if shift {
        a.push(format!("{KEY_LEFTSHIFT}:1"));
    }
    a.extend([format!("{code}:1"), format!("{code}:0")]);
    if shift {
        a.push(format!("{KEY_LEFTSHIFT}:0"));
    }
    a.push(format!("{KEY_LEFTCTRL}:0"));
    a
}

/// Ctrl+<letter> (c/v) через wtype/ydotool. `None` — сессия не Wayland, вызывающий
/// жмёт клавиши сам (enigo / X11).
pub fn wayland_chord(letter: char, shift: bool) -> Option<Result<()>> {
    if session() != Session::Wayland {
        return None;
    }
    let Some(code) = evdev_letter(letter) else {
        return Some(Err(anyhow!("неподдерживаемая клавиша {letter}")));
    };
    Some(first_ok(vec![
        (&WTYPE, wtype_chord_args(letter, shift)),
        (&YDOTOOL, ydotool_chord_args(code, shift)),
    ]))
}

pub fn wayland_type(text: &str) -> Option<Result<()>> {
    if session() != Session::Wayland {
        return None;
    }
    Some(first_ok(vec![
        (&WTYPE, vec!["--".to_string(), text.to_string()]),
        (&YDOTOOL, vec!["type".into(), "--".into(), text.to_string()]),
    ]))
}

pub fn wayland_backspaces(count: usize) -> Option<Result<()>> {
    if session() != Session::Wayland {
        return None;
    }
    if count == 0 {
        return Some(Ok(()));
    }
    let mut wtype = Vec::new();
    let mut ydotool = s(&["key"]);
    for _ in 0..count {
        wtype.extend(s(&["-k", "BackSpace"]));
        ydotool.extend([format!("{KEY_BACKSPACE}:1"), format!("{KEY_BACKSPACE}:0")]);
    }
    Some(first_ok(vec![(&WTYPE, wtype), (&YDOTOOL, ydotool)]))
}

// ───────────────────────────── X11: активное окно ─────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActiveWindow {
    pub id: String,
    pub class: String,
    pub title: String,
}

fn xprop(args: &[&str]) -> Option<String> {
    let out = Command::new("xprop")
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// `_NET_ACTIVE_WINDOW(WINDOW): window id # 0x4600003` -> `0x4600003`.
fn parse_active_window_id(text: &str) -> Option<String> {
    let id = text.split('#').nth(1)?.trim();
    (id.starts_with("0x") && id != "0x0").then(|| id.to_string())
}

fn quoted_values(line: &str) -> Vec<String> {
    line.split('"')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// Разбор вывода `xprop -id <id> WM_CLASS _NET_WM_NAME`: (класс, заголовок).
fn parse_window_props(text: &str) -> (String, String) {
    let mut class = String::new();
    let mut title = String::new();
    for line in text.lines() {
        if line.starts_with("WM_CLASS") {
            // ("instance", "Class"): нужен второй, он стабильнее.
            class = quoted_values(line).pop().unwrap_or_default();
        } else if line.starts_with("_NET_WM_NAME") {
            title = quoted_values(line).into_iter().next().unwrap_or_default();
        }
    }
    (class, title)
}

/// Активное окно X11 через `xprop`. На Wayland и без xprop — `None`.
pub fn x11_active_window() -> Option<ActiveWindow> {
    if session() != Session::X11 {
        return None;
    }
    if find_in_path(XPROP.bin).is_none() {
        static HINT: std::sync::Once = std::sync::Once::new();
        HINT.call_once(|| {
            log::info!(
                "xprop не найден: контекст приложения и Ctrl+Shift+V в терминалах недоступны ({})",
                install_hint(&XPROP)
            );
        });
        return None;
    }
    let id = parse_active_window_id(&xprop(&["-root", "_NET_ACTIVE_WINDOW"])?)?;
    let (class, title) = parse_window_props(&xprop(&["-id", &id, "WM_CLASS", "_NET_WM_NAME"])?);
    Some(ActiveWindow { id, class, title })
}

const TERMINAL_MARKERS: &[&str] = &[
    "gnome-terminal",
    "konsole",
    "xterm",
    "urxvt",
    "rxvt",
    "alacritty",
    "kitty",
    "tilix",
    "terminator",
    "xfce4-terminal",
    "mate-terminal",
    "lxterminal",
    "qterminal",
    "wezterm",
    "foot",
    "ghostty",
    "terminology",
    "guake",
    "yakuake",
    "sakura",
    "st-256color",
    "hyper",
    "termite",
    "cool-retro-term",
    "blackbox",
    "ptyxis",
    "kgx",
];

pub fn is_terminal_class(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    TERMINAL_MARKERS.iter().any(|m| class.contains(m))
}

/// В фокусе терминал X11: вставка там — Ctrl+Shift+V (Ctrl+V шлёт управляющий символ).
pub fn terminal_active() -> bool {
    x11_active_window().is_some_and(|w| is_terminal_class(&w.class))
}

// ───────────────────────────── системный звук (PulseAudio / PipeWire) ─────────────────────────────

fn parse_mute(output: &str) -> Result<bool> {
    // "Mute: yes" / "Mute: no"; локализация pactl отключена через LC_ALL=C ниже.
    match output.trim().rsplit(' ').next() {
        Some("yes") => Ok(true),
        Some("no") => Ok(false),
        _ => Err(anyhow!("pactl: неожиданный ответ {output:?}")),
    }
}

fn pactl(args: &[&str]) -> Result<String> {
    if find_in_path(PACTL.bin).is_none() {
        return Err(anyhow!(
            "pactl не найден (PulseAudio/PipeWire): {}",
            install_hint(&PACTL)
        ));
    }
    let out = Command::new("pactl")
        .args(args)
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| anyhow!("pactl: {e}"))?;
    if !out.status.success() {
        return Err(anyhow!(
            "pactl: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn sink_muted() -> Result<bool> {
    parse_mute(&pactl(&["get-sink-mute", "@DEFAULT_SINK@"])?)
}

pub fn set_sink_mute(mute: bool) -> Result<()> {
    pactl(&[
        "set-sink-mute",
        "@DEFAULT_SINK@",
        if mute { "1" } else { "0" },
    ])
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_detection() {
        assert_eq!(
            session_from(Some("wayland"), None, Some(":0")),
            Session::Wayland
        );
        assert_eq!(
            session_from(Some("x11"), Some("wayland-0"), None),
            Session::X11
        );
        assert_eq!(
            session_from(Some("tty"), Some("wayland-0"), None),
            Session::Wayland
        );
        assert_eq!(session_from(None, Some(""), Some(":0")), Session::X11);
        assert_eq!(session_from(None, None, None), Session::Unknown);
    }

    #[test]
    fn package_manager_from_os_release() {
        let mint = "NAME=\"Linux Mint\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
        assert_eq!(package_manager_from(mint), PackageManager::Apt);
        assert_eq!(package_manager_from("ID=fedora\n"), PackageManager::Dnf);
        assert_eq!(
            package_manager_from("ID=manjaro\nID_LIKE=arch\n"),
            PackageManager::Pacman
        );
        assert_eq!(
            package_manager_from("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n"),
            PackageManager::Zypper
        );
        assert_eq!(package_manager_from("ID=nixos\n"), PackageManager::Unknown);
        assert_eq!(
            install_command(&XPROP, PackageManager::Pacman),
            "sudo pacman -S xorg-xprop"
        );
    }

    #[test]
    fn chord_arguments() {
        assert_eq!(
            wtype_chord_args('v', true),
            ["-M", "ctrl", "-M", "shift", "-k", "v", "-m", "shift", "-m", "ctrl"]
        );
        assert_eq!(
            ydotool_chord_args("47", false),
            ["key", "29:1", "47:1", "47:0", "29:0"]
        );
        assert_eq!(
            ydotool_chord_args("46", true),
            ["key", "29:1", "42:1", "46:1", "46:0", "42:0", "29:0"]
        );
    }

    #[test]
    fn xprop_parsing() {
        assert_eq!(
            parse_active_window_id("_NET_ACTIVE_WINDOW(WINDOW): window id # 0x4600003\n"),
            Some("0x4600003".into())
        );
        assert_eq!(parse_active_window_id("... window id # 0x0"), None);
        let props = "WM_CLASS(STRING) = \"gnome-terminal-server\", \"Gnome-terminal\"\n\
                     _NET_WM_NAME(UTF8_STRING) = \"user@host: ~\"\n";
        let (class, title) = parse_window_props(props);
        assert_eq!(class, "Gnome-terminal");
        assert_eq!(title, "user@host: ~");
        assert!(is_terminal_class(&class));
        assert!(!is_terminal_class("Firefox"));
    }

    #[test]
    fn mute_parsing() {
        assert!(parse_mute("Mute: yes\n").unwrap());
        assert!(!parse_mute("Mute: no\n").unwrap());
        assert!(parse_mute("???").is_err());
    }
}
