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
pub fn apply(ov: &tauri::WebviewWindow) -> bool {
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
    // A fixed, stretched surface gives pointer events stable local coordinates.
    // Only the DOM capsule moves. Native input shape leaves the rest click-through.
    win.set_resizable(true);
    win.set_default_size(0, 0);
    for edge in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
        win.set_anchor(edge, true);
        win.set_layer_shell_margin(edge, 0);
    }
    if !win.is_layer_window() {
        log::warn!("плашка: layer-shell не включился, остаётся обычное окно");
        return false;
    }
    true
}

/// Only the capsule receives input; transparent WebView margins pass through.
#[cfg(target_os = "linux")]
pub fn set_hit(ov: &tauri::WebviewWindow, rect: (f64, f64, f64, f64)) -> bool {
    use gtk::prelude::*;
    use gtk_layer_shell::LayerShell;
    let Ok(win) = ov.gtk_window() else {
        return false;
    };
    let region = hit_region(rect);
    win.input_shape_combine_region(Some(&region));
    win.is_layer_window()
}

#[cfg(target_os = "linux")]
fn hit_region((x, y, w, h): (f64, f64, f64, f64)) -> gtk::cairo::Region {
    let region = gtk::cairo::Region::create();
    if [x, y, w, h].iter().all(|v| v.is_finite()) && w > 0.0 && h > 0.0 {
        let left = x.floor().max(0.0) as i32;
        let top = y.floor().max(0.0) as i32;
        let right = (x + w).ceil().max(f64::from(left)) as i32;
        let bottom = (y + h).ceil().max(f64::from(top)) as i32;
        let _ = region.union_rectangle(&gtk::cairo::RectangleInt::new(
            left,
            top,
            right.saturating_sub(left),
            bottom.saturating_sub(top),
        ));
    }
    region
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn input_region_passes_transparent_window_margins_through() {
        let region = hit_region((111.5, 38.5, 44.0, 14.0));
        assert!(region.contains_point(133, 45));
        for (x, y) in [(10, 45), (90, 45), (133, 10), (133, 75)] {
            assert!(
                !region.contains_point(x, y),
                "transparent pixel ({x},{y}) captured"
            );
        }
        assert!(hit_region((f64::NAN, 0.0, 20.0, 20.0)).is_empty());
        assert!(hit_region((0.0, 0.0, -1.0, 20.0)).is_empty());
    }

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
