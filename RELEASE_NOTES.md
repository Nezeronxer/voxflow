# VoxFlow v1.0.3

## Highlights

- GitHub Releases updater: VoxFlow can check `Nezeronxer/voxflow` for the latest release, show an update toast, download `VoxFlow-Setup-*.exe`, and launch the installer after user confirmation.
- Startup auto-check is enabled by default and can be toggled in `Управление`; manual `Проверить` and `Установить` controls are available there too.
- Multilingual local ASR defaults: fresh installs now prefer `language=auto`, `whisper_server`, and the multilingual Whisper q5 model.
- Cloud STT now receives a compact recognition-bias prompt with active app label, recent dictation tail, project terms, user dictionary, snippet triggers, and learned corrections.
- Final cloud STT is cancelled before the network call if the target window changed, reducing wrong-target and privacy risk.
- Installer polish: dedicated branded setup icon and improved small-icon legibility.

## Release Artifact

- `VoxFlow-Setup-1.0.3.exe`
- Size: `288162743` bytes
- SHA256: `820D69AB02B807A1073D622CCAB59D1A17DB050380A24A13637E038CB0A66BAA`

## Verified For This Build

- `npm run build` — OK.
- `npm audit --audit-level=high` — OK, 0 vulnerabilities.
- `cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check` — OK.
- `cargo test --manifest-path src-tauri\Cargo.toml --lib` — OK, 140 passed, 5 ignored.
- `cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings` — OK.
- `npm run tauri -- build --no-bundle` — OK, release exe built as `voxflow v1.0.3`.
- Inno Setup 6.7.1 compile — OK, created `VoxFlow-Setup-1.0.3.exe`.
- Silent per-user install — OK, registry version `1.0.3`, installed exe product version `1.0.3`, installed exe SHA256 `284124AAC4C8D51A5EB0BB8A8A76B730E4BF0D758A367A706826D5DF5277895A`.
- `git diff --check` — OK.

## Known Limits

- Some ASR e2e tests are opt-in and ignored by default because they require local models, private WAV fixtures, or network/proxy access.
- Recognition quality depends on selected language, model, microphone, and provider. If dictation is wrong, change language/model first; BYOK cloud STT can help, but online/API providers depend on network, quota, and service availability.
- The Windows installer is unsigned unless a signing certificate is added.
- The GitHub Actions installer workflow still needs a committed or downloaded runtime-resource step before it can build fully on a clean runner; this release was built and uploaded from the verified local Windows release environment.
