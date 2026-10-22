# Что уже построено

Читается каждым исполнителем до начала работы. Не изобретай заново то, что здесь есть.
Подробная карта кода с файл:строка - `notes.md` в этой же папке.

## Границы, решённые в спецификации

| Модуль | Владеет | Выставляет | Прячет |
|---|---|---|---|
| хелпер сессии ORT (таск 01, новый `fn` в одном месте, например `ort_session.rs` или в `gigaam.rs`) | опции onnxruntime | `fn session_from_file(path: &Path, threads: usize) -> Result<Session>`; вызывают gigaam.rs, parakeet.rs, vad.rs | какие именно опции включены |
| `engine.rs` выгрузка (таск 02) | жизнь моделей в памяти | `fn unload_asr_models(ctx: &EngineCtx, reason: &str)`; отметка активности `fn touch_activity(ctx)`; поток простоя | таймер, порядок локов |
| `settings.rs` (таск 02) | новые поля | `asr_unload_idle: bool` (true), `asr_unload_after_min: u32` (10, clamp 1-1440) | `#[serde(default)]` |
| `local_llm.rs` (таск 02) | llama-server | функция остановки сервера, которую зовёт выгрузка; ленивый перезапуск при следующем запросе | процесс |
| `list_models` / `ModelInfo` (таск 03) | размер и использование | новые поля `disk_mb: Option<u64>` (null - не скачана), `in_use: bool` в Rust и `types.ts` | правила маршрута |
| `delete_model` (таск 03) | удаление | после удаления зовёт `unload_asr_models` для этой модели | - |
| `release-linux.yml` (таск 04) | пакет Arch | ассет `VoxFlow-X.Y.Z-linux-x64.pkg.tar.zst` в `SHA256SUMS-linux.txt` | PKGBUILD |
| `updater.rs` Linux (таск 04) | выбор и установка на Arch | на Arch при установке pacman - `.pkg.tar.zst`, иначе как было; остальные платформы **не менять** | `pkexec` |

Швы для тестов: юнит-тесты `settings.rs`, `engine.rs` (выгрузка/простой без настоящих моделей), `updater.rs`
(выбор ассета), `models.rs`; замер памяти и точности - `#[ignore]`-тест с `VOXFLOW_TEST_DATA_DIR`.

## Общие правила проекта

- Tauri 2 + Rust (`voxflow/src-tauri/`) + React/TS/Vite (`voxflow/`).
- Rust: `cd voxflow/src-tauri && cargo test --locked --lib 2>&1 | tail -30`; `cargo clippy --locked --all-targets -- -D warnings`; `cargo fmt` перед сдачей (CI гоняет `--check`).
- Фронт: `cd voxflow && npx tsc --noEmit && node --test tests/*.test.mjs && npx vite build`. `node` вне PATH: `export PATH="$PATH:/Users/nezeronxer/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin"`.
- Модельные тесты видят настоящие модели только с `VOXFLOW_TEST_DATA_DIR="$HOME/Library/Application Support/VoxFlow"`; записи - `.../VoxFlow/dataset/*.wav`. Модели пользователя не удалять и не менять.
- Linux-код (`cfg(target_os="linux")`) на маке не компилируется - его проверит CI; код только для не-Linux помечать `cfg_attr(not(target_os = "linux"), allow(dead_code))` при нужде.
- Боевой GigaAM для русского - полная fp32 (D-040, решение пользователя 22.10 «полную, но ужать»), int8 запасная. Каталог моделей не менять.
- Версии и заметки релиза меняет только таск 05. Файлы `* (… conflicted copy …)*` не трогать. `.autopilot/` не трогать.
- Не коммитить, не пушить - это делает оркестратор. Никаких упоминаний ИИ в коде и текстах.
- Не хватает зависимости/инструмента - не ставь сам, верни `BLOCKED` с названием.
- Комментарии и тексты для пользователя - по-русски, короткое тире «-». Модуль Rust начинается с `//!` «зачем».
- Решение с неочевидной причиной - запись D-0xx в DECISIONS.md (что / почему / отвергнуто), следующий свободный номер D-053.
