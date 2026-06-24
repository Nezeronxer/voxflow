# VoxFlow v1.0 Local QA

Дата: 2026-06-24

## Артефакты

- Release exe: `voxflow\src-tauri\target\release\voxflow.exe`
- ProductVersion: `1.0.0`
- FileVersion: `1.0.0`
- Installer: `installer\Output\VoxFlow-Setup-1.0.0.exe`
- Installer size: `287908802` bytes
- Installer LastWriteTime: `2026-06-24 16:30:39`

## Проверено

- `npm run build` — OK.
- `cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check` — OK.
- `cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings` — OK.
- `cargo test --manifest-path src-tauri\Cargo.toml --lib` — OK, 99 passed, 5 ignored.
- `npm run tauri -- build --no-bundle` — OK, release exe собран как `voxflow v1.0.0`.
- Inno Setup compile — OK, создан `VoxFlow-Setup-1.0.0.exe`.
- Windows version metadata у release exe — OK: ProductVersion/FileVersion `1.0.0`.

## Release Notes

- Релизная версия проекта зафиксирована как `v1.0` для пользователя и `1.0.0` в semver-метаданных.
- Voice-guided prompt rewrite входит в релиз: пользователь пишет базовый prompt, диктует инструкцию, получает preview и вручную применяет или отменяет результат.
- Локальный режим остаётся дефолтным; cloud STT и rewrite работают только в BYOK-сценарии.
- Инсталлятор per-user, без требования прав администратора, с сохранением пользовательских данных в `%LOCALAPPDATA%\VoxFlow`.

## Остаточные риски

- Установщик не подписан кодовым сертификатом.
- Некоторые e2e ASR-тесты остаются ignored, потому что требуют локальные модели, приватные WAV fixtures или сеть.
- GitHub release builds требуют, чтобы runtime resources были доступны в репозитории или подставлялись отдельным download-step.
