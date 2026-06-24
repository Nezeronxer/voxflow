# VoxFlow v1.0.1

## Highlights

- Local-first Windows dictation with hold-to-talk, toggle mode and double-press latch.
- Dictation overlay hotfix: after silence the blue pill shows the settled processed text, and the final insertion waits behind a visible `Готовлю` spinner.
- Final ASR hotfix: long silence is compacted before final recognition, and rare stale/hallucinated tails are rejected when they do not match the live preview.
- App-aware rewrite profiles for chats, mail, AI prompts, code and documents.
- Voice-guided prompt rewrite: write a base prompt, speak an edit instruction, preview and apply the rewritten result.
- BYOK cloud STT and rewrite providers, including OpenRouter/OpenAI-compatible options.
- Per-app prompt rules for AI tools such as Codex, ChatGPT, Claude, Gemini, DeepSeek, Grok and OpenRouter.
- Auto-mute restore guard and safer app shutdown behavior.
- Polished `Облако` settings UI with provider status, preset grid and local/cloud clarity.
- Installer theme polish: the close-running-apps/preparing page, finished page and task list are readable in the dark neon wizard.

## Local QA Gate

Run from `voxflow`:

```powershell
npm ci
npm run build
npm audit --audit-level=high
cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check
cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path src-tauri\Cargo.toml --lib
npm run tauri -- build --no-bundle
```

Then build the installer from the repository root:

```powershell
& "C:\Program Files (x86)\Inno Setup 6\ISCC.exe" ".\installer\VoxFlow.iss"
```

## Release Artifact

- `VoxFlow-Setup-1.0.1.exe`
- Size: `288045771` bytes
- SHA256: `0EE3551C9A2AE784250D450D389F091DC8CC1F14AFDAE42CD58827D7C65E716D`

## Verified For This Build

- `npm run build` — OK.
- `npm audit --audit-level=high` — OK, 0 vulnerabilities.
- `cargo fmt --manifest-path src-tauri\Cargo.toml --all -- --check` — OK.
- `cargo clippy --manifest-path src-tauri\Cargo.toml --all-targets -- -D warnings` — OK.
- `cargo test --manifest-path src-tauri\Cargo.toml --lib -- --nocapture` — OK, 115 passed, 5 ignored.
- `npm run tauri -- build --no-bundle` — OK, release exe built as `voxflow v1.0.1`.
- Inno Setup compile — OK, created `VoxFlow-Setup-1.0.1.exe`.
- Silent per-user install — OK, registry and installed resources verified under `%LOCALAPPDATA%\VoxFlow`.
- Installer visual QA — OK, welcome and additional-tasks screens checked in the Russian wizard.

## Known Limits

- Some ASR e2e tests are opt-in and ignored by default because they require local models, private WAV fixtures or network/proxy access.
- Recognition quality depends on the selected language, model, microphone and provider. If dictation is wrong, change the language/model first; if local STT is unstable for a user, they can enable BYOK cloud STT, but online/API providers may be affected by network, rate limits and service availability.
- The Windows installer is unsigned unless a signing certificate is added to the release pipeline.
- GitHub release builds require bundled runtime resources to be available in the repository or supplied by a future download step.
