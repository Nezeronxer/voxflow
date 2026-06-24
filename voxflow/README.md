# VoxFlow App

Tauri + React desktop app for VoxFlow. The repository root contains the user
README, installer script and release workflow; this folder contains the app
source.

## Local QA

```powershell
npm ci
npm run build
cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri\Cargo.toml --lib
npm run tauri -- build --no-bundle
```

Build the Inno installer from the repository root after the packaged Tauri
build:

```powershell
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" ".\installer\VoxFlow.iss"
```

Do not build release installers from a plain `cargo build --release` binary:
that path can point WebView2 at the dev server. Use `npm run tauri -- build
--no-bundle` first.
