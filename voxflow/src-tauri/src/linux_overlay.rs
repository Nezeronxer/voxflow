//! Linux: плашка как layer surface (wlr-layer-shell) на Wayland.
//!
//! Обычное окно на Wayland игнорирует `always_on_top`, уходит под другие окна и
//! может забрать фокус. Layer surface слоя Overlay держится поверх всего и
//! фокус не берёт. Где протокола нет (X11, GNOME) — окно остаётся как было.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use crate::linux_env::Session;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlayMode {
    LayerShell,
    Plain,
}

/// Режим окна плашки: layer-shell только на Wayland и только если композитор
/// его поддерживает.
pub fn pick_mode(session: Session, layer_shell_supported: bool) -> OverlayMode {
    if session == Session::Wayland && layer_shell_supported {
        OverlayMode::LayerShell
    } else {
        OverlayMode::Plain
    }
}

/// Делает окно плашки layer surface, если сессия и композитор это позволяют.
/// Звать до первого `show()`; возвращает `true`, когда слой задан.
#[cfg(target_os = "linux")]
pub fn apply(ov: &tauri::WebviewWindow, bottom_margin: i32) -> bool {
    use gtk::prelude::*;
    use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

    let mode = pick_mode(crate::linux_env::session(), gtk_layer_shell::is_supported());
    if mode != OverlayMode::LayerShell {
        return false;
    }
    let Ok(win) = ov.gtk_window() else {
        return false;
    };
    // init_layer_shell отказывается работать с уже реализованным окном.
    if win.is_realized() {
        win.hide();
        win.unrealize();
    }
    win.init_layer_shell();
    win.set_namespace("voxflow-overlay");
    win.set_layer(Layer::Overlay);
    win.set_keyboard_mode(KeyboardMode::None);
    win.set_exclusive_zone(0);
    // Якорь только снизу: по горизонтали композитор центрирует поверхность сам.
    for edge in [Edge::Left, Edge::Right, Edge::Top] {
        win.set_anchor(edge, false);
    }
    win.set_anchor(Edge::Bottom, true);
    win.set_layer_shell_margin(Edge::Bottom, bottom_margin);
    if !win.is_layer_window() {
        log::warn!("плашка: layer-shell не включился, остаётся обычное окно");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wayland_with_protocol_gets_layer_shell() {
        assert_eq!(pick_mode(Session::Wayland, true), OverlayMode::LayerShell);
    }

    #[test]
    fn everything_else_keeps_plain_window() {
        for (session, supported) in [
            (Session::Wayland, false), // GNOME без layer-shell
            (Session::X11, true),      // XWayland-сессия или X11 с чужим дисплеем
            (Session::X11, false),
            (Session::Unknown, true),
            (Session::Unknown, false),
        ] {
            assert_eq!(pick_mode(session, supported), OverlayMode::Plain);
        }
    }
}
