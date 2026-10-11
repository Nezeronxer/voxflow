# Что уже построено

Читается каждым исполнителем до начала работы. Не изобретай заново то, что здесь есть.

## Границы, решённые в спецификации

| Модуль | Владеет | Выставляет | Прячет |
|---|---|---|---|
| `engine` (FinalQueue + поколения) | порядок вставок, судьба черновика | существующие `final_still_wanted`, `FinalQueue::{enter, wait_turn}`; новый вход: ожидание хода перед печатью превью | как черновик передаётся финалу |
| `overlay` (`setup_overlay` в lib.rs + новый `linux_overlay`) | окно плашки | `setup_overlay(app)` без изменения сигнатуры | layer-shell, проверку поддержки |
| CI/релиз | workflow, версии | — | — |

Швы для тестов: unit-тесты в `engine.rs` (FinalQueue, final_still_wanted) — тот же шов, что в 2.2.0.
Оверлей: юнит-тест только на чистую функцию выбора режима; само окно — живой запуск.

## Общие правила проекта

- Корень репо: `/home/nezeronxer/Моя папка/wispr flow`. Приложение — `voxflow/`, бэкенд — `voxflow/src-tauri/src/`. Tauri 2, Rust, React/TS (Vite).
- Машина: Arch Linux, Hyprland (Wayland). wtype, ydotool, gtk-layer-shell 0.10.1, webkit2gtk-4.1 стоят.
- Тесты Rust: `cd voxflow/src-tauri && cargo test --locked --lib 2>&1 | tail -30`; один тест — `cargo test --locked --lib <имя>`.
  Гейты: `cargo clippy --locked --all-targets -- -D warnings`, `cargo fmt --all -- --check`.
  Рантайм-ресурсы для build.rs: `bash script/build_linux_runtime.sh` из корня (если build.rs ругается на ресурсы).
- Фронтенд: `cd voxflow && npm run check` (test + build).
- Не трогать: пользовательские настройки на диске, `~/.config/hypr`, запущенный Handy.
- Нет зависимости — добавлять крейт можно только если таск это прямо разрешает; иначе вернуть `BLOCKED` с названием.
- Никаких следов ИИ в коде, комментариях, коммитах. Комментарии по-русски, как в окружающем коде.
