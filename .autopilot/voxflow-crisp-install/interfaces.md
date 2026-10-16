# Что уже построено

Читается каждым исполнителем до начала работы. Не изобретай заново то, что здесь есть.

## Границы, решённые в спецификации

| Модуль | Владеет | Выставляет | Прячет |
|---|---|---|---|
| `script/setup-macos-signing.sh` (таск 01) | создание постоянного ключа подписи | запуск без аргументов; итог - identity `VoxFlow Self-Signed` (codeSigning) в login Keychain + секреты GitHub `MACOS_SELF_SIGNED_P12_BASE64`, `MACOS_SELF_SIGNED_P12_PASSWORD` | openssl, пароль p12 (генерируется, не печатается) |
| `script/build_macos_arm64_release.sh` + `.github/workflows/release-macos-arm64.yml` (таск 01) | подпись и имена ассетов | env `VOXFLOW_SIGNING_IDENTITY`; режимы developer-id-notarized / signed-unnotarized (самоподпись) / adhoc. Самоподпись даёт ассеты `VoxFlow-macOS-X.Y.Z-arm64-signed-unnotarized.dmg` и `.app.zip` | импорт p12 во временный keychain |
| `updater.rs` | выбор и установка ассета | уже принимает `-arm64-signed-unnotarized.dmg` во всех версиях с 2.0.1 - **не менять** | - |
| `script/install-macos.sh` (таск 02) | установка для людей | `curl -fsSL https://raw.githubusercontent.com/Nezeronxer/voxflow/master/script/install-macos.sh \| bash` | всё |
| `macos_permissions.rs` + фронт (таск 03) | доступы | существующие функции + новые Tauri-команды статуса/запроса | AVFoundation FFI |
| модели `models.rs`, `gigaam.rs`, `final_eval.rs` (таск 04) | каталог и загрузка GigaAM, замер точности | существующие `DirModel`, `final_accuracy_eval` | выбор fp32/int8 |

Швы для тестов: существующие юнит-тесты `updater.rs`, `models.rs`, `macos_permissions.rs`;
`final_eval.rs` (`#[ignore]`); shell-скрипты - `bash -n` и `shellcheck` (если установлен).

## Общие правила проекта

- Tauri 2 + Rust (`voxflow/src-tauri/`) + React/TS/Vite (`voxflow/`). Bundle id `com.nezeronxer.voxflow.macos`.
- Rust: `cd voxflow/src-tauri && cargo test --locked --lib 2>&1 | tail -30`; `cargo clippy --locked --all-targets -- -D warnings`; `cargo fmt` перед коммитом (CI гоняет `cargo fmt --check`). Один тест: `cargo test --locked --lib <имя>`.
- Фронт: `cd voxflow && npx tsc --noEmit && node --test tests/*.test.mjs && npx vite build`. Если `node` нет в PATH: `/Users/nezeronxer/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/bin/node`.
- Модельные тесты видят настоящие модели только с `VOXFLOW_TEST_DATA_DIR="$HOME/Library/Application Support/VoxFlow"`; записи - `.../VoxFlow/dataset/*.wav`.
- Версии и заметки релиза меняет только таск 05. Файлы `* (… conflicted copy …)*` не трогать. `.autopilot/` не трогать.
- Не коммитить: коммиты делает оркестратор. Не пушить. Никаких упоминаний ИИ в коде и текстах.
- Не хватает зависимости/инструмента - не ставь сам, верни `BLOCKED` с названием.
- Секреты (ключи, пароли, p12) никогда не печатать в вывод, не писать в репозиторий и в ответ.
- Тексты для пользователя - по-русски, короткое тире «-», без канцелярита.
