# VoxFlow v1.0.4

## Highlights

- Security hardening release for the Tauri/Rust backend after a Codex Security deep scan.
- Hardened loopback URL validation so `127.*` hostnames no longer bypass local-only checks.
- Switched curl multipart scalar fields to `--form-string` for STT requests to prevent `@file` upload semantics.
- Added origin-only validation and percent-encoding for Deepgram query parameters.
- Forced local whisper-server and Ollama traffic to bypass app/env proxies with `--noproxy *`.
- Restricted Ollama to loopback URLs to preserve the local/offline backend contract.
- Routed Gemini through the app proxy setting while keeping proxy credentials out of argv.
- Redacted proxy userinfo before renderer settings emission and stopped preserving endpoint-bound BYOK keys when provider endpoints change.
- Rejected path traversal and absolute paths in local model filenames.
- Made personalization and cloud live draft opt-in defaults for fresh settings.

## Release Artifact

- `VoxFlow-Setup-1.0.4.exe`
- Built by the GitHub Actions installer workflow from tag `v1.0.4`.

## Verified For This Build

- `cargo fmt` — OK.
- `TAURI_CONFIG={"bundle":{"resources":[]}} cargo test --lib` — OK, 147 passed, 5 ignored.
- Codex Security deep scan completed: 22 reportable findings were triaged; this release remediates the directly code-fixable injection, loopback/proxy, secret-routing, model path, and privacy-default issues.

## Known Limits

- Some ASR e2e tests are opt-in and ignored by default because they require local models, private WAV fixtures, or network/proxy access.
- A plain local `cargo test --lib` in a clone without bundled whisper resources can fail before tests because `tauri.conf.json` references installer resources; the CI release workflow expects those runtime resources to be present.
- Release installer/model artifact signature pinning and at-rest encryption for local settings/history remain follow-up product/infrastructure work.
- The Windows installer is unsigned unless a signing certificate is added.
