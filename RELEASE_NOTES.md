# VoxFlow v1.0.7

## Highlights

- Fixed intermittent final-text insertion failures on Windows 10 by sending paste/copy shortcuts through native `SendInput` virtual-key events instead of the cross-platform `enigo` chord path.
- Kept the existing safe clipboard flow: VoxFlow still writes the recognized text to clipboard, preserves the no-double-inject invariant after `Ctrl+V`, and leaves the final dictation text available for manual paste if the target app still refuses the shortcut.
- The change is scoped to Windows `Ctrl+V`/`Ctrl+C` shortcut emission; non-Windows paths and text typing behavior are unchanged.

## Release Artifact

- `VoxFlow-Setup-1.0.7.exe`
- Size: `289082143` bytes
- SHA256: `29195CCE0FD4DD1BC51B0D47B1B68AD2F1BA9796B0CA3FED28B00EFF97DE4DE0`
- Built from tag `v1.0.7` in the verified local Windows release environment.

## Verified For This Build

- `cargo fmt --manifest-path src-tauri\Cargo.toml --check` — OK.
- `cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings` — OK.
- `cargo test --manifest-path src-tauri\Cargo.toml --lib` — OK, 147 passed, 5 ignored.
- `npm run build` — OK.
- `npm run tauri -- build --no-bundle` — OK, release exe built as `voxflow v1.0.7`.
- Inno Setup 6.7.1 compile — OK, created `VoxFlow-Setup-1.0.7.exe`.
- Installed-app Windows QA — OK: silent install over `%LOCALAPPDATA%\\VoxFlow`, installed exe hash matched release exe, registry `Version=1.0.7`, app launched as `%LOCALAPPDATA%\\VoxFlow\\voxflow.exe`.
- Windows paste smoke — OK: clipboard text pasted into a fresh Notepad tab via `Ctrl+V`; text was visible and verified.
- `git diff --check` — OK.

## Known Limits

- Some ASR e2e tests are opt-in and ignored by default because they require local models, private WAV fixtures, or network/proxy access.
- GitHub Actions installer workflow still needs an explicit runtime-resource provisioning step; clean GitHub checkout does not contain ignored `src-tauri/resources` assets required by Tauri resource globs.
- The Windows installer is unsigned unless a signing certificate is added.
