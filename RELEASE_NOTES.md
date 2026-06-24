# VoxFlow v1.0

## Highlights

- Local-first Windows dictation with hold-to-talk, toggle mode and double-press latch.
- App-aware rewrite profiles for chats, mail, AI prompts, code and documents.
- Voice-guided prompt rewrite: write a base prompt, speak an edit instruction, preview and apply the rewritten result.
- BYOK cloud STT and rewrite providers, including OpenRouter/OpenAI-compatible options.
- Per-app prompt rules for AI tools such as Codex, ChatGPT, Claude, Gemini, DeepSeek, Grok and OpenRouter.
- Auto-mute restore guard and safer app shutdown behavior.
- Polished `Облако` settings UI with provider status, preset grid and local/cloud clarity.

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

- `VoxFlow-Setup-1.0.0.exe`
- SHA256: `BF585D85BE46F0A8ADD39E097EB6892831C21E7D1094F973F659AB0C3ECB993A`

## Known Limits

- Some ASR e2e tests are opt-in and ignored by default because they require local models, private WAV fixtures or network/proxy access.
- Recognition quality depends on the selected language, model, microphone and provider. If dictation is wrong, change the language/model first; if local STT is unstable for a user, they can enable BYOK cloud STT, but online/API providers may be affected by network, rate limits and service availability.
- The Windows installer is unsigned unless a signing certificate is added to the release pipeline.
- GitHub release builds require bundled runtime resources to be available in the repository or supplied by a future download step.
