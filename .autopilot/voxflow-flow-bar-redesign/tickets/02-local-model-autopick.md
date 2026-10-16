# 02 Самовыбор установленной локальной модели + model в status
Требования: R05. Спецификация §3 (бэкенд-часть), §4. Интерфейс: interfaces.md.
Файлы: `voxflow/src-tauri/src/models.rs`, `engine.rs`, при нужде `settings.rs`.
Готово: чистая функция выбора + юнит-тесты на каждый случай §4; `model` в status; `cargo test --locked --lib`, clippy -D warnings, fmt.
