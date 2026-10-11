# VoxFlow 2.0 app

Tauri 2 + React 19 desktop app for macOS ARM64, Windows x64 and Linux x64. Product,
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
python3 script/check_versions.py --tag v2.2.0
```

## Packaging

Do not publish a binary from plain `cargo build --release`: use the Tauri build
so the compiled frontend is embedded and the correct platform config is merged.
Runtime resources are intentionally ignored in Git and must come from the
checksum-verified release scripts/workflows.

- Tag release orchestrator: `.github/workflows/release.yml`
- Windows reusable/manual artifact builder: `.github/workflows/build-installer.yml`
- macOS ARM64 reusable/manual artifact builder: `.github/workflows/release-macos-arm64.yml`
- Linux x64 reusable/manual artifact builder (deb, rpm, AppImage): `.github/workflows/release-linux.yml`
- Cross-platform PR gate: `.github/workflows/ci.yml`

The platform builders never publish a release independently. The tag
orchestrator verifies all artifact sets and their checksums in a draft, then
publishes the shared GitHub Release only after the complete set is present.

Windows uses the existing per-user Inno installer after `tauri build
--no-bundle`. macOS produces a thin ARM64 app/DMG with checksum-pinned Whisper
and VAD resources. With no Apple secrets it is completely ad-hoc sealed and
strict-verified. With a Developer ID certificate plus one complete Apple ID or
App Store Connect notarization credential set, the same workflow produces and
verifies Developer ID signed, notarized, and stapled artifacts. Partial secret
sets fail instead of silently publishing an untrusted artifact.

## Linux

Linux x64 ships as `.deb` (Debian, Ubuntu, Mint), `.rpm` (Fedora, RHEL,
openSUSE) and `.AppImage` (everything else, including Arch/Manjaro). The
packages come from the release page; `deb`/`rpm` pull in webkit2gtk-4.1, GTK 3,
ALSA and the Ayatana AppIndicator tray library automatically.
ONNX Runtime (GigaAM, Parakeet, Silero VAD) is linked statically by the `ort`
crate, so nothing extra is installed for it. Whisper (`whisper-cli`) is built
from the pinned whisper.cpp sources by `script/build_linux_runtime.sh`, CPU only
and needs an AVX2 CPU. NVIDIA/CUDA acceleration is Windows-only.

Data lives in `~/.local/share/VoxFlow` (`$XDG_DATA_HOME`), autostart is a
`~/.config/autostart/*.desktop` entry written by the Settings toggle. The tray
icon needs an AppIndicator host (GNOME: the "AppIndicator and KStatusNotifierItem
Support" extension).

### Build dependencies

```bash
# Debian / Ubuntu / Mint
sudo apt install build-essential cmake pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libasound2-dev libssl-dev libx11-dev \
  libxtst-dev libxi-dev libevdev-dev libwayland-dev patchelf
# Fedora / RHEL
sudo dnf install gcc-c++ cmake pkgconf-pkg-config webkit2gtk4.1-devel gtk3-devel \
  libayatana-appindicator-gtk3-devel librsvg2-devel alsa-lib-devel openssl-devel \
  libX11-devel libXtst-devel libXi-devel libevdev-devel wayland-devel patchelf
# Arch / Manjaro
sudo pacman -S --needed base-devel cmake webkit2gtk-4.1 gtk3 libayatana-appindicator \
  librsvg alsa-lib openssl libx11 libxtst libxi libevdev wayland patchelf
# openSUSE
sudo zypper install gcc-c++ cmake pkg-config webkit2gtk3-devel gtk3-devel \
  libayatana-appindicator3-devel librsvg-devel alsa-devel libopenssl-devel \
  libX11-devel libXtst-devel libXi-devel libevdev-devel wayland-devel patchelf
```

Then `./script/build_linux_runtime.sh` once, and `npx tauri build` in `voxflow/`.

### Optional runtime tools

VoxFlow works without them but loses the feature in brackets. A missing tool
produces an error naming the package for your distribution.

| Tool | Package (apt / dnf / pacman / zypper) | For |
|---|---|---|
| `xprop` | x11-utils / xorg-x11-utils / xorg-xprop / xprop | X11: active window, Ctrl+Shift+V in terminals |
| `pactl` | pulseaudio-utils / pulseaudio-utils / libpulse / pulseaudio-utils | mute system output while dictating (PulseAudio and PipeWire) |
| `wtype` | wtype (all four) | Wayland text insertion on Sway, Hyprland, river |
| `ydotool` | ydotool (all four) | Wayland text insertion on GNOME and KDE (needs `ydotoold` and access to `/dev/uinput`) |

### X11

Everything works out of the box: the global hotkey (hold to talk, double tap to
latch) comes from XRecord, text is inserted with the clipboard plus Ctrl+V
(Ctrl+Shift+V when a known terminal is focused, which needs `xprop`) and the
previous clipboard is restored afterwards.

### Wayland

Wayland deliberately forbids global key capture and synthetic input, so:

- **Hotkey.** VoxFlow reads `/dev/input/event*` directly (hold to talk works).
  This needs membership in the `input` group:
  `sudo usermod -aG input $USER`, then log out and in. Without it the app tells
  you so and keeps running. Fallback that needs no permissions: bind a system
  shortcut to a command, a second `voxflow` start only forwards the command to
  the running instance:
  - `voxflow --toggle` starts/stops dictation (any desktop environment);
  - `voxflow --start` and `voxflow --stop` for compositors with press and
    release bindings (Hyprland `bind` + `bindr`, Sway `bindsym` +
    `--release`) give hold to talk;
  - `voxflow --cancel` aborts a dictation.
  For an AppImage use `/path/to/VoxFlow.AppImage --toggle`. The XDG
  GlobalShortcuts portal is not used.
- **Insertion.** Needs `wtype` (Sway, Hyprland, river) or `ydotool` (GNOME,
  KDE); the clipboard is handled through the `wlr-data-control` protocol where
  the compositor has it and through XWayland otherwise. Terminals are not
  detected on Wayland, so use Ctrl+V-compatible terminal settings or the
  `type` insertion method there.
- **Window context.** Wayland hides other windows, so per-app tone profiles and
  the "window changed, do not paste" guard are unavailable.
- **Overlay.** Always-on-top and free positioning of the overlay depend on the
  compositor; GNOME ignores them. The overlay is never click-through on Linux.

### Updates

The AppImage replaces itself with the verified release asset
(`VoxFlow-<version>-linux-x64.AppImage`) and restarts. For `deb`/`rpm` the
Settings button opens the release page: install the new package with `apt`,
`dnf` or `zypper`.

## Recognition routes

- Explicit Russian: GigaAM v3.
- Explicit English: Parakeet TDT v3 when installed.
- Auto/mixed language and universal fallback: Whisper, with Large v3 Turbo Q5
  recommended. Turbo Q8/full and Large v3 remain available; the weak
  Tiny/Base/Small/Medium models were removed (saved choice migrates to Q5).
- Cloud STT/rewrite: optional BYOK; local fallback preserves the selected local
  router.
