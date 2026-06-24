# VoxFlow v1.0.1 Local QA

Дата: 2026-06-24

## Артефакты

- Release exe: `voxflow\src-tauri\target\release\voxflow.exe`
- ProductVersion: `1.0.1`
- FileVersion: `1.0.1`
- Installer: `installer\Output\VoxFlow-Setup-1.0.1.exe`
- Installer size: `288045771` bytes
- Installer SHA256: `0EE3551C9A2AE784250D450D389F091DC8CC1F14AFDAE42CD58827D7C65E716D`
- Installer LastWriteTime: `2026-06-24`

## Проверено

- `npm run build` — OK.
- `npm audit --audit-level=high` — OK, 0 vulnerabilities.
- `cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check` — OK.
- `cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings` — OK.
- `cargo test --manifest-path src-tauri\Cargo.toml --lib -- --nocapture` — OK, 115 passed, 5 ignored.
- `npm run tauri -- build --no-bundle` — OK, release exe собран как `voxflow v1.0.1`.
- Inno Setup compile — OK, создан `VoxFlow-Setup-1.0.1.exe`.
- Silent per-user install — OK, registry version `1.0.1`, whisper/CUDA/VAD resources на месте.
- Installer visual QA — OK, welcome и additional-tasks screens читаемы в тёмной теме.

## Release Notes

- Релизная версия проекта зафиксирована как `v1.0.1` для пользователя и `1.0.1` в semver-метаданных.
- Voice-guided prompt rewrite входит в релиз: пользователь пишет базовый prompt, диктует инструкцию, получает preview и вручную применяет или отменяет результат.
- Dictation overlay hotfix входит в релиз: settled preview после паузы, spinner `Готовлю` во время финальной вставки и защита от редких stale/hallucinated final tails.
- Локальный режим остаётся дефолтным; cloud STT и rewrite работают только в BYOK-сценарии.
- Инсталлятор per-user, без требования прав администратора, с сохранением пользовательских данных в `%LOCALAPPDATA%\VoxFlow`.

## Остаточные риски

- Установщик не подписан кодовым сертификатом.
- Некоторые e2e ASR-тесты остаются ignored, потому что требуют локальные модели, приватные WAV fixtures или сеть.
- GitHub release builds требуют, чтобы runtime resources были доступны в репозитории или подставлялись отдельным download-step.
