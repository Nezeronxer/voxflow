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
python3 script/check_versions.py --tag v2.0.1
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
--no-bundle`. macOS produces a DMG whose complete app bundle is ad-hoc signed
and strict-verified in secret-free CI. Developer ID/notarization and Windows
Authenticode still require owner-provided certificates and are not represented
as successful when those secrets are absent.

## Recognition routes

- Russian: GigaAM v3.
- Auto/25 supported languages: Parakeet TDT v3 when installed.
- Universal fallback: Whisper Tiny, Base, Small, Medium, Large v3 Turbo
  Q5/Q8/full, or Large v3.
- Cloud STT/rewrite: optional BYOK; local fallback preserves the selected local
  router.
