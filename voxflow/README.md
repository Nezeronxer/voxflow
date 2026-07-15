# VoxFlow 2.0 app

Tauri 2 + React 19 desktop app for macOS ARM64 and Windows x64. Product,
installation and license details live in the repository-root
[README](../README.md).

## Local frontend QA

```bash
npm ci
npm test
npm run build
npm audit --audit-level=high
```

The browser build contains a deterministic local demo bridge for visual and
interaction QA. Native Tauri builds continue to use the Rust command/event
bridge; no demo data is selected in that runtime.

## Rust QA

From the repository root:

```bash
cargo fmt --manifest-path voxflow/src-tauri/Cargo.toml --all -- --check
cargo clippy --locked --manifest-path voxflow/src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path voxflow/src-tauri/Cargo.toml --lib
python3 script/check_versions.py --tag v2.0.8
```

## Packaging

Do not publish a binary from plain `cargo build --release`: use the Tauri build
so the compiled frontend is embedded and the correct platform config is merged.
Runtime resources are intentionally ignored in Git and must come from the
checksum-verified release scripts/workflows.

- Tag release orchestrator: `.github/workflows/release.yml`
- Windows reusable/manual artifact builder: `.github/workflows/build-installer.yml`
- macOS ARM64 reusable/manual artifact builder: `.github/workflows/release-macos-arm64.yml`
- Cross-platform PR gate: `.github/workflows/ci.yml`

The two platform builders never publish a release independently. The tag
orchestrator verifies both artifact sets and their checksums in a draft, then
publishes the shared GitHub Release only after the complete set is present.

Windows uses the existing per-user Inno installer after `tauri build
--no-bundle`. macOS produces a thin ARM64 app/DMG with checksum-pinned Whisper
and VAD resources. With no Apple secrets it is completely ad-hoc sealed and
strict-verified. With a Developer ID certificate plus one complete Apple ID or
App Store Connect notarization credential set, the same workflow produces and
verifies Developer ID signed, notarized, and stapled artifacts. Partial secret
sets fail instead of silently publishing an untrusted artifact.

## Recognition routes

- Explicit Russian: GigaAM v3.
- Explicit English: Parakeet TDT v3 when installed.
- Auto/mixed language and universal fallback: Whisper, with Large v3 Turbo Q5
  recommended. Medium, Turbo Q8/full, and Large v3 remain available;
  Tiny/Base/Small are hidden legacy choices unless already installed or active.
- Cloud STT/rewrite: optional BYOK; local fallback preserves the selected local
  router.
