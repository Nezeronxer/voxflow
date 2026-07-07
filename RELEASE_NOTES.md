# VoxFlow v1.0.8

## Highlights

- Fixed a regression-looking Russian dictation path on Windows where fresh/default settings (`language=auto`, `engine=whisper_server`) could keep using Whisper auto even when the stronger Russian GigaAM model was available.
- VoxFlow now auto-downloads GigaAM for Russian/auto local setups and uses it as the final Russian fallback when Whisper auto returns the wrong language or a weaker Russian transcript.
- The GitHub Actions release workflow now provisions ignored runtime resources itself: whisper.cpp v1.8.6 CPU/CUDA sidecars and Silero VAD are fetched during the Windows build before packaging the Inno installer.

## Release Artifact

- `VoxFlow-Setup-1.0.8.exe`
- Built from tag `v1.0.8` by the GitHub Actions Windows installer workflow.

## Verified For This Build

- `cargo fmt --manifest-path voxflow/src-tauri/Cargo.toml --all -- --check` — OK on the local macOS checkout.
- `git diff --check` — OK.
- Full Rust tests, clippy, frontend build, Windows Tauri build, Inno packaging, installer asset size, and SHA256 are produced by the tag workflow.

## Known Limits

- Some ASR e2e tests are opt-in and ignored by default because they require local models, private WAV fixtures, or network/proxy access.
- The Windows installer is unsigned unless a signing certificate is added.
