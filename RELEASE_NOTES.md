# VoxFlow v1.0.5

## Highlights

- Restored the bright canonical VoxFlow app icon for Windows taskbar, window, tray and shortcuts.
- Synchronized generated branding assets into `src-tauri/icons` so future Tauri/Inno builds cannot accidentally embed the dark unreadable icon set.
- Kept the installer `setup.exe` icon separate from the app icon: setup stays installer-branded, while `voxflow.exe` uses the readable green/blue speech-bubble mark.

## Release Artifact

- `VoxFlow-Setup-1.0.5.exe`
- Size: `288222498` bytes
- SHA256: `90F1C29E13B5D602E511C04C4C9F952A2DB023571CDDFE378437EDFE4DD22BE4`
- Built from tag `v1.0.5` in the verified local Windows release environment.

## Verified For This Build

- `voxflow/branding/build_icon.py` regenerated canonical app icon assets and synced them into `voxflow/src-tauri/icons`.
- Extracted icon from the release `voxflow.exe` resolves to the bright app icon instead of the dark low-contrast icon seen in v1.0.4.
- `npm run build` — OK.
- `npm audit --audit-level=high` — OK, 0 vulnerabilities.
- `cargo fmt` — OK.
- `TAURI_CONFIG={"bundle":{"resources":[]}} cargo test --lib` — OK, 147 passed, 5 ignored.
- `cargo clippy --all-targets -- -D warnings` — OK.
- `npm run tauri -- build --no-bundle` — OK.
- Inno Setup 6.7.1 compile — OK.

## Known Limits

- Some ASR e2e tests are opt-in and ignored by default because they require local models, private WAV fixtures, or network/proxy access.
- GitHub Actions installer workflow still needs an explicit runtime-resource provisioning step; clean GitHub checkout does not contain ignored `src-tauri/resources` assets required by Tauri resource globs.
- The Windows installer is unsigned unless a signing certificate is added.
