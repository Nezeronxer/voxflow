//! Linux/Wayland: глобальный перехват клавиш чтением /dev/input/event* (evdev).
//!
//! На Wayland композитор не отдаёт чужие нажатия приложениям, а rdev слушает
//! только X11. Ядро же отдаёт события клавиатур напрямую, если пользователь
//! состоит в группе `input`. События преобразуются в `rdev::Event`, поэтому
//! hold-to-talk / double-tap работают тем же кодом, что на X11. Без прав
//! остаются системный шорткат на `voxflow --toggle` и пара `--start` / `--stop`.
//!
//! Только std: struct input_event читается руками (24 байта на 64-битных ОС).
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use rdev::{Event, EventType, Key};
use std::io::Read;
use std::sync::Arc;
use std::time::SystemTime;

#[cfg(target_pointer_width = "64")]
const EVENT_SIZE: usize = 24;
#[cfg(not(target_pointer_width = "64"))]
const EVENT_SIZE: usize = 16;

const EV_KEY: u16 = 1;

pub type Callback = Arc<dyn Fn(Event) + Send + Sync>;

/// Коды linux/input-event-codes.h -> rdev::Key (только клавиши, которые умеет
/// назначать `hotkey::parse_key`).
pub fn key_from_code(code: u16) -> Option<Key> {
    Some(match code {
        1 => Key::Escape,
        2 => Key::Num1,
        3 => Key::Num2,
        4 => Key::Num3,
        5 => Key::Num4,
        6 => Key::Num5,
        7 => Key::Num6,
        8 => Key::Num7,
        9 => Key::Num8,
        10 => Key::Num9,
        11 => Key::Num0,
        12 => Key::Minus,
        13 => Key::Equal,
        14 => Key::Backspace,
        15 => Key::Tab,
        16 => Key::KeyQ,
        17 => Key::KeyW,
        18 => Key::KeyE,
        19 => Key::KeyR,
        20 => Key::KeyT,
        21 => Key::KeyY,
        22 => Key::KeyU,
        23 => Key::KeyI,
        24 => Key::KeyO,
        25 => Key::KeyP,
        26 => Key::LeftBracket,
        27 => Key::RightBracket,
        28 => Key::Return,
        29 => Key::ControlLeft,
        30 => Key::KeyA,
        31 => Key::KeyS,
        32 => Key::KeyD,
        33 => Key::KeyF,
        34 => Key::KeyG,
        35 => Key::KeyH,
        36 => Key::KeyJ,
        37 => Key::KeyK,
        38 => Key::KeyL,
        39 => Key::SemiColon,
        40 => Key::Quote,
        41 => Key::BackQuote,
        42 => Key::ShiftLeft,
        43 => Key::BackSlash,
        44 => Key::KeyZ,
        45 => Key::KeyX,
        46 => Key::KeyC,
        47 => Key::KeyV,
        48 => Key::KeyB,
        49 => Key::KeyN,
        50 => Key::KeyM,
        51 => Key::Comma,
        52 => Key::Dot,
        53 => Key::Slash,
        54 => Key::ShiftRight,
        55 => Key::KpMultiply,
        56 => Key::Alt,
        57 => Key::Space,
        58 => Key::CapsLock,
        59 => Key::F1,
        60 => Key::F2,
        61 => Key::F3,
        62 => Key::F4,
        63 => Key::F5,
        64 => Key::F6,
        65 => Key::F7,
        66 => Key::F8,
        67 => Key::F9,
        68 => Key::F10,
        69 => Key::NumLock,
        70 => Key::ScrollLock,
        71 => Key::Kp7,
        72 => Key::Kp8,
        73 => Key::Kp9,
        74 => Key::KpMinus,
        75 => Key::Kp4,
        76 => Key::Kp5,
        77 => Key::Kp6,
        78 => Key::KpPlus,
        79 => Key::Kp1,
        80 => Key::Kp2,
        81 => Key::Kp3,
        82 => Key::Kp0,
        83 => Key::KpDelete,
        86 => Key::IntlBackslash,
        87 => Key::F11,
        88 => Key::F12,
        96 => Key::KpReturn,
        97 => Key::ControlRight,
        98 => Key::KpDivide,
        99 => Key::PrintScreen,
        100 => Key::AltGr,
        102 => Key::Home,
        103 => Key::UpArrow,
        104 => Key::PageUp,
        105 => Key::LeftArrow,
        106 => Key::RightArrow,
        107 => Key::End,
        108 => Key::DownArrow,
        109 => Key::PageDown,
        110 => Key::Insert,
        111 => Key::Delete,
        119 => Key::Pause,
        125 => Key::MetaLeft,
        126 => Key::MetaRight,
        _ => return None,
    })
}

/// Разбор одной записи input_event: (код, нажата?). Автоповтор (value 2) и не-клавиши — `None`.
fn parse_event(buf: &[u8; EVENT_SIZE]) -> Option<(u16, bool)> {
    let ty = u16::from_ne_bytes([buf[EVENT_SIZE - 8], buf[EVENT_SIZE - 7]]);
    let code = u16::from_ne_bytes([buf[EVENT_SIZE - 6], buf[EVENT_SIZE - 5]]);
    let value = i32::from_ne_bytes([
        buf[EVENT_SIZE - 4],
        buf[EVENT_SIZE - 3],
        buf[EVENT_SIZE - 2],
        buf[EVENT_SIZE - 1],
    ]);
    if ty != EV_KEY {
        return None;
    }
    match value {
        0 => Some((code, false)),
        1 => Some((code, true)),
        _ => None,
    }
}

/// Номера event-устройств, у которых в /proc/bus/input/devices есть обработчик `kbd`.
fn keyboard_event_nodes(devices: &str) -> Vec<String> {
    let mut nodes = Vec::new();
    for block in devices.split("\n\n") {
        let Some(handlers) = block.lines().find_map(|l| l.strip_prefix("H: Handlers=")) else {
            continue;
        };
        let words: Vec<&str> = handlers.split_whitespace().collect();
        if !words.contains(&"kbd") {
            continue;
        }
        nodes.extend(
            words
                .iter()
                .filter(|w| w.starts_with("event"))
                .map(|w| format!("/dev/input/{w}")),
        );
    }
    nodes
}

fn read_loop(path: String, callback: Callback) {
    let mut file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            log::debug!("evdev {path}: {e}");
            return;
        }
    };
    let mut buf = [0u8; EVENT_SIZE];
    while file.read_exact(&mut buf).is_ok() {
        let Some((code, pressed)) = parse_event(&buf) else {
            continue;
        };
        let Some(key) = key_from_code(code) else {
            continue;
        };
        callback(Event {
            time: SystemTime::now(),
            name: None,
            event_type: if pressed {
                EventType::KeyPress(key)
            } else {
                EventType::KeyRelease(key)
            },
        });
    }
    log::warn!("evdev {path}: поток событий закончился (устройство отключено?)");
}

/// Запускает читателей на всех клавиатурах и блокируется, пока живёт хотя бы один.
/// `Err` — ни одно устройство не открылось (обычно нет прав на /dev/input).
pub fn run(callback: Callback) -> Result<(), String> {
    let devices = std::fs::read_to_string("/proc/bus/input/devices")
        .map_err(|e| format!("не удалось прочитать /proc/bus/input/devices: {e}"))?;
    let nodes = keyboard_event_nodes(&devices);
    let mut handles = Vec::new();
    let mut denied = false;
    for node in nodes {
        match std::fs::File::open(&node) {
            Ok(_) => {}
            Err(e) => {
                denied |= e.kind() == std::io::ErrorKind::PermissionDenied;
                continue;
            }
        }
        let cb = Arc::clone(&callback);
        if let Ok(h) = std::thread::Builder::new()
            .name("voxflow-evdev".into())
            .spawn(move || read_loop(node, cb))
        {
            handles.push(h);
        }
    }
    if handles.is_empty() {
        return Err(if denied {
            "Wayland: нет доступа к /dev/input. Добавьте себя в группу input \
             (sudo usermod -aG input $USER, затем перелогиньтесь) или назначьте в настройках \
             системы шорткат на команду «voxflow --toggle»."
                .into()
        } else {
            "Wayland: клавиатуры в /dev/input не найдены. Назначьте в настройках системы \
             шорткат на команду «voxflow --toggle»."
                .into()
        });
    }
    for h in handles {
        let _ = h.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_events_only() {
        let mut buf = [0u8; EVENT_SIZE];
        buf[EVENT_SIZE - 8..EVENT_SIZE - 6].copy_from_slice(&EV_KEY.to_ne_bytes());
        buf[EVENT_SIZE - 6..EVENT_SIZE - 4].copy_from_slice(&97u16.to_ne_bytes());
        buf[EVENT_SIZE - 4..].copy_from_slice(&1i32.to_ne_bytes());
        assert_eq!(parse_event(&buf), Some((97, true)));
        buf[EVENT_SIZE - 4..].copy_from_slice(&0i32.to_ne_bytes());
        assert_eq!(parse_event(&buf), Some((97, false)));
        buf[EVENT_SIZE - 4..].copy_from_slice(&2i32.to_ne_bytes());
        assert_eq!(parse_event(&buf), None);
        buf[EVENT_SIZE - 8..EVENT_SIZE - 6].copy_from_slice(&2u16.to_ne_bytes());
        assert_eq!(parse_event(&buf), None);
        assert_eq!(key_from_code(97), Some(Key::ControlRight));
    }

    #[test]
    fn finds_keyboards_in_proc_listing() {
        let text = "I: Bus=0011 Vendor=0001\nN: Name=\"AT Translated Set 2 keyboard\"\n\
                    H: Handlers=sysrq kbd event3 leds\nB: EV=120013\n\n\
                    I: Bus=0003\nN: Name=\"Mouse\"\nH: Handlers=mouse0 event4\nB: EV=17\n";
        assert_eq!(keyboard_event_nodes(text), ["/dev/input/event3"]);
    }
}
