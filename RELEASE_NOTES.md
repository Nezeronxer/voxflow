# VoxFlow v1.0.6

## Highlights

- Fixed the custom Inno Setup window/taskbar icon so the installer no longer appears as a generic Windows process on the taskbar.
- Kept the installed VoxFlow app icon path separate from the setup icon path: this release only changes installer branding/runtime icon handling.
- Refreshed Inno modern wizard assets with DPI-sized 24-bit BMPs, including square `WizardSmallImageFile` frames for current Windows scaling.

## Release Artifact

- `VoxFlow-Setup-1.0.6.exe`
- Size: `289070173` bytes
- SHA256: `0154BE0B745B592BA0CC8FE9B2ABB052878516C8D2D767E292204E604A65F6E8`
- Built from tag `v1.0.6` in the verified local Windows release environment.

## Verified For This Build

- `npm run build` — OK.
- `npm audit --audit-level=high` — OK, 0 vulnerabilities.
- `cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check` — OK.
- `TAURI_CONFIG={"bundle":{"resources":[]}} cargo test --manifest-path src-tauri\Cargo.toml --lib` — OK, 147 passed, 5 ignored.
- `cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings` — OK.
- `npm run tauri -- build --no-bundle` — OK, release exe built as `voxflow v1.0.6`.
- Inno Setup 6.7.1 compile — OK, created `VoxFlow-Setup-1.0.6.exe`.
- Runtime setup-icon probe — OK: launched setup without installing; `WM_GETICON` returned nonzero small/big icon handles for `VoxFlow-Setup-1.0.6.tmp`.
- `git diff --check` — OK.

## Known Limits

- Some ASR e2e tests are opt-in and ignored by default because they require local models, private WAV fixtures, or network/proxy access.
- GitHub Actions installer workflow still needs an explicit runtime-resource provisioning step; clean GitHub checkout does not contain ignored `src-tauri/resources` assets required by Tauri resource globs.
- The Windows installer is unsigned unless a signing certificate is added.
