# 02 — Плашка поверх окон на Wayland

**Требования:** R04i, R04i.1, R04i.2
**Blocked by:** —
**Зона:** `voxflow/src-tauri/src/lib.rs` (только `setup_overlay` и регистрация модуля), новый `voxflow/src-tauri/src/linux_overlay.rs`, `voxflow/src-tauri/Cargo.toml` + `Cargo.lock` (новая зависимость), `voxflow/src-tauri/tauri.linux.conf.json`, раздел Linux в `voxflow/README.md`, `.github/workflows/release-linux.yml` и `ci.yml` только в части apt-пакетов
**Волна:** 1
**Status:** ready

## Что должно заработать

На Wayland окно плашки сейчас обычный toplevel: `set_always_on_top` там игнорируется, плашка уходит
под окна и может попасть в тайлинг/забрать фокус. Нужно: на Linux при Wayland-сессии и
поддержке протокола (`gtk_layer_shell::is_supported()`) сделать окно оверлея layer surface —
слой OVERLAY, keyboard mode None, якорь низ, по центру, отступ снизу как сейчас, exclusive zone 0 —
до первого `show()`. Иначе (X11, GNOME без layer-shell, Windows, macOS) — текущее поведение.

## Из брифа, дословно

> «доделай мне voxflow»
> (открытый хвост 2.2.0: «на Wayland плашка не всегда держится поверх окон»)

## Разделы спецификации

Истории 11–13, Решения §4, Границы: `overlay`.

## Критерии приёмки

- [ ] Зависимость `gtk-layer-shell` (крейт, совместимый с gtk 0.18, которую тянет Tauri 2) только под `cfg(target_os = "linux")`
- [ ] Включение только при Wayland (`linux_env::session()`) и `is_supported()`; иначе прежний путь — проверяется юнит-тестом на функции выбора режима
- [ ] Прозрачность окна сохраняется; слой задан до show()
- [ ] Пакетам deb/rpm добавлена runtime-зависимость (deb: `libgtk-layer-shell0`, rpm: `gtk-layer-shell`), в README Linux — сборочные пакеты (`libgtk-layer-shell-dev` и аналоги) и строчка, что на Hyprland/Sway/KDE плашка теперь поверх окон; CI/релиз Linux ставят `libgtk-layer-shell-dev`
- [ ] `cargo clippy --all-targets -- -D warnings`, `cargo test --lib`, `cargo fmt --check` зелёные на Linux
