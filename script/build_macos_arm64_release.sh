#!/usr/bin/env bash
set -euo pipefail

# Reproducible-input macOS release builder for VoxFlow.
#
# The default path is intentionally unsigned: it passes --no-sign to Tauri and
# names the artifacts accordingly. A local machine may opt into Developer ID
# signing by setting VOXFLOW_SIGNING_IDENTITY to an identity already installed
# in the current keychain. This script never imports certificates or handles
# notarization credentials.

readonly TARGET_TRIPLE="aarch64-apple-darwin"
readonly TARGET_ARCH="arm64"
readonly MIN_MACOS_VERSION="11.0"

readonly WHISPER_VERSION="1.8.6"
readonly WHISPER_COMMIT="23ee03506a91ac3d3f0071b40e66a430eebdfa1d"
readonly WHISPER_SOURCE_DATE_EPOCH="1780314980"
readonly WHISPER_ARCHIVE_SHA256="c8b0de473e9ec47a74bdf6104425c709261beeada8d6d7c1fec7432be701d032"
readonly WHISPER_ARCHIVE_URL="https://codeload.github.com/ggml-org/whisper.cpp/tar.gz/${WHISPER_COMMIT}"
readonly WHISPER_MACOS11_PATCH="script/patches/whisper.cpp-v1.8.6-macos11.patch"
readonly WHISPER_MACOS11_PATCH_SHA256="8ebc327129c2d5e2e970fc06080c6218034d412f2dcc8499544ffdab2a45ffe7"
readonly WHISPER_PATCHED_METAL_SHA256="276500e84ede862135a8bfd430f616f4e88daacaa5f04287f19028f0c37edbdd"

readonly SILERO_COMMIT="b163605b3f44c3aadf28f97b125a2f7c461e9a7f"
readonly SILERO_SHA256="1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
readonly SILERO_URL="https://raw.githubusercontent.com/snakers4/silero-vad/${SILERO_COMMIT}/src/silero_vad/data/silero_vad.onnx"

readonly CMAKE_VERSION="3.31.6"
readonly CMAKE_SHA256="330b9514f5112e5ed4fb08b8b05803b776fd9b539a6ae12927d14dcc0ee2ba8d"
readonly CMAKE_URL="https://github.com/Kitware/CMake/releases/download/v${CMAKE_VERSION}/cmake-${CMAKE_VERSION}-macos-universal.tar.gz"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT_DIR/voxflow"
TAURI_DIR="$APP_DIR/src-tauri"
RESOURCES_DIR="$TAURI_DIR/resources"
ARM_RUNTIME_DIR="$RESOURCES_DIR/whisper-darwin-arm64"
VAD_DIR="$RESOURCES_DIR/vad"
TARGET_RELEASE_DIR="$TAURI_DIR/target/$TARGET_TRIPLE/release"
APP_BUNDLE="$TARGET_RELEASE_DIR/bundle/macos/VoxFlow.app"
OUTPUT_ROOT="${VOXFLOW_RELEASE_DIR:-$APP_DIR/release/macos}"
RELEASE_TAG=""
RUNTIME_ONLY=0

TEMP_ROOT=""
MOUNT_POINT=""

usage() {
  cat >&2 <<'EOF'
Usage: script/build_macos_arm64_release.sh [--tag vX.Y.Z] [--runtime-only]

Environment:
  VOXFLOW_RELEASE_DIR         Output parent (default: voxflow/release/macos)
  VOXFLOW_SIGNING_IDENTITY    Optional installed Developer ID identity.
                              Omit for the CI-default unsigned build.
  VOXFLOW_XCODE_VERSION       Optional exact Xcode version gate (for CI).
  VOXFLOW_XCODE_BUILD         Optional exact Xcode build gate (for CI).
EOF
}

die() {
  echo "error: $*" >&2
  exit 1
}

cleanup() {
  if [[ -n "$MOUNT_POINT" && -d "$MOUNT_POINT" ]]; then
    /usr/bin/hdiutil detach "$MOUNT_POINT" -quiet >/dev/null 2>&1 || true
  fi
  if [[ -n "$TEMP_ROOT" && -d "$TEMP_ROOT" ]]; then
    rm -rf "$TEMP_ROOT"
  fi
}
trap cleanup EXIT

while [[ $# -gt 0 ]]; do
  case "$1" in
    --tag)
      [[ $# -ge 2 ]] || die "--tag requires a value"
      RELEASE_TAG="$2"
      shift 2
      ;;
    --runtime-only)
      RUNTIME_ONLY=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage
      die "unknown argument: $1"
      ;;
  esac
done

require_command() {
  command -v "$1" >/dev/null 2>&1 || die "required command is missing: $1"
}

sha256_of() {
  /usr/bin/shasum -a 256 "$1" | awk '{print $1}'
}

assert_sha256() {
  local path="$1"
  local expected="$2"
  local actual
  actual="$(sha256_of "$path")"
  [[ "$actual" == "$expected" ]] || die "SHA256 mismatch for $path: expected $expected, got $actual"
}

download_verified() {
  local url="$1"
  local expected="$2"
  local destination="$3"
  /usr/bin/curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --retry 3 --retry-all-errors --connect-timeout 30 --max-time 1800 \
    --output "$destination" "$url"
  assert_sha256 "$destination" "$expected"
}

assert_arm64_macho() {
  local binary="$1"
  local archs
  [[ -f "$binary" && -x "$binary" ]] || die "missing executable: $binary"
  /usr/bin/file "$binary" | grep -q 'Mach-O 64-bit executable arm64' \
    || die "not a thin ARM64 Mach-O executable: $binary"
  archs="$(/usr/bin/lipo -archs "$binary")"
  [[ "$archs" == "$TARGET_ARCH" ]] || die "unexpected architectures in $binary: $archs"
}

assert_minos_at_most() {
  local binary="$1"
  local values
  local value
  local saw_macos=0
  values="$(/usr/bin/xcrun vtool -show-build "$binary" | awk '$1 == "platform" && $2 == "MACOS" { seen = 1 } $1 == "minos" { print $2 } END { if (seen != 1) exit 7 }')" \
    || die "missing macOS LC_BUILD_VERSION in $binary"
  [[ -n "$values" ]] || die "missing minimum macOS version in $binary"
  while IFS= read -r value; do
    [[ -n "$value" ]] || continue
    saw_macos=1
    python3 - "$value" "$MIN_MACOS_VERSION" <<'PY'
import sys

def version(value: str) -> tuple[int, ...]:
    return tuple(int(part) for part in value.split("."))

actual, maximum = map(version, sys.argv[1:])
if actual > maximum:
    raise SystemExit(f"minimum macOS {sys.argv[1]} exceeds {sys.argv[2]}")
PY
  done <<< "$values"
  [[ "$saw_macos" == 1 ]] || die "no macOS minimum version found in $binary"
}

assert_system_only_dependencies() {
  local binary="$1"
  local dependency
  while IFS= read -r dependency; do
    [[ -n "$dependency" ]] || continue
    case "$dependency" in
      /usr/lib/*|/System/Library/Frameworks/*) ;;
      *) die "non-system dynamic dependency in $binary: $dependency" ;;
    esac
  done < <(/usr/bin/otool -L "$binary" | tail -n +2 | awk '{print $1}')
}

validate_native_binary() {
  assert_arm64_macho "$1"
  assert_minos_at_most "$1"
  assert_system_only_dependencies "$1"
}

assert_runner() {
  [[ "$(uname -s)" == "Darwin" ]] || die "this release must be built on macOS"
  [[ "$(uname -m)" == "$TARGET_ARCH" ]] || die "this release requires a native ARM64 runner"
  for command in awk cargo curl file lipo otool python3 rustc shasum tar xcrun; do
    require_command "$command"
  done
  if [[ -n "${DEVELOPER_DIR:-}" ]]; then
    [[ -x "$DEVELOPER_DIR/usr/bin/xcodebuild" ]] || die "Xcode is missing at $DEVELOPER_DIR"
  else
    require_command xcodebuild
  fi

  if [[ -n "${VOXFLOW_XCODE_VERSION:-}" ]]; then
    local actual_xcode
    actual_xcode="$(/usr/bin/xcodebuild -version | awk 'NR == 1 {print $2}')"
    [[ "$actual_xcode" == "$VOXFLOW_XCODE_VERSION" ]] \
      || die "Xcode $VOXFLOW_XCODE_VERSION required, got $actual_xcode"
  fi
  if [[ -n "${VOXFLOW_XCODE_BUILD:-}" ]]; then
    local actual_xcode_build
    actual_xcode_build="$(/usr/bin/xcodebuild -version | awk 'NR == 2 {print $3}')"
    [[ "$actual_xcode_build" == "$VOXFLOW_XCODE_BUILD" ]] \
      || die "Xcode build $VOXFLOW_XCODE_BUILD required, got $actual_xcode_build"
  fi
}

developer_tools_description() {
  local details
  local clt_version
  details="$(/usr/bin/xcodebuild -version 2>/dev/null | tr '\n' ' ' | sed 's/ $//' || true)"
  if [[ -n "$details" ]]; then
    printf '%s\n' "$details"
    return
  fi
  clt_version="$(/usr/sbin/pkgutil --pkg-info=com.apple.pkg.CLTools_Executables 2>/dev/null \
    | awk -F': ' '$1 == "version" { print $2 }' || true)"
  [[ -n "$clt_version" ]] || clt_version="unknown"
  printf 'Command Line Tools %s\n' "$clt_version"
}

assert_tauri_cli() {
  [[ -x "$APP_DIR/node_modules/@tauri-apps/cli/tauri.js" ]] \
    || die "Tauri CLI is missing after npm ci"
}

version_gate() {
  if [[ -n "$RELEASE_TAG" ]]; then
    python3 "$ROOT_DIR/script/check_versions.py" --tag "$RELEASE_TAG"
  else
    python3 "$ROOT_DIR/script/check_versions.py"
  fi
}

frontend_gates() {
  (
    cd "$APP_DIR"
    npm ci
    npm test
    npm run build
    npm audit --audit-level=high
  )
}

prepare_pinned_cmake() {
  local archive="$TEMP_ROOT/cmake.tar.gz"
  local cmake_root="$TEMP_ROOT/cmake"
  download_verified "$CMAKE_URL" "$CMAKE_SHA256" "$archive"
  mkdir -p "$cmake_root"
  /usr/bin/tar -xzf "$archive" -C "$cmake_root" --strip-components=1
  CMAKE_BIN="$cmake_root/CMake.app/Contents/bin/cmake"
  [[ -x "$CMAKE_BIN" ]] || die "pinned CMake executable is missing"
  [[ "$($CMAKE_BIN --version | awk 'NR == 1 {print $3}')" == "$CMAKE_VERSION" ]] \
    || die "unexpected CMake version"
}

prepare_runtime() {
  local whisper_archive="$TEMP_ROOT/whisper.cpp.tar.gz"
  local whisper_source="$TEMP_ROOT/whisper.cpp"
  local whisper_build="$TEMP_ROOT/whisper-build"
  local vad_model="$TEMP_ROOT/silero_vad.onnx"
  local jobs
  local reproducible_flags

  prepare_pinned_cmake
  download_verified "$WHISPER_ARCHIVE_URL" "$WHISPER_ARCHIVE_SHA256" "$whisper_archive"
  download_verified "$SILERO_URL" "$SILERO_SHA256" "$vad_model"

  mkdir -p "$whisper_source"
  /usr/bin/tar -xzf "$whisper_archive" -C "$whisper_source" --strip-components=1
  [[ -s "$whisper_source/CMakeLists.txt" ]] || die "whisper.cpp source extraction failed"
  assert_sha256 "$ROOT_DIR/$WHISPER_MACOS11_PATCH" "$WHISPER_MACOS11_PATCH_SHA256"
  /usr/bin/patch --directory="$whisper_source" --strip=1 --fuzz=0 \
    < "$ROOT_DIR/$WHISPER_MACOS11_PATCH"
  assert_sha256 "$whisper_source/ggml/src/ggml-metal/ggml-metal-device.m" \
    "$WHISPER_PATCHED_METAL_SHA256"

  export MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS_VERSION"
  jobs="$(/usr/sbin/sysctl -n hw.logicalcpu)"
  reproducible_flags="-Werror=unguarded-availability-new -ffile-prefix-map=$TEMP_ROOT=/voxflow-build -fdebug-prefix-map=$TEMP_ROOT=/voxflow-build"
  SOURCE_DATE_EPOCH="$WHISPER_SOURCE_DATE_EPOCH" \
  "$CMAKE_BIN" -S "$whisper_source" -B "$whisper_build" \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_C_FLAGS="$reproducible_flags" \
    -DCMAKE_CXX_FLAGS="$reproducible_flags" \
    -DCMAKE_OSX_ARCHITECTURES="$TARGET_ARCH" \
    -DCMAKE_OSX_DEPLOYMENT_TARGET="$MIN_MACOS_VERSION" \
    -DBUILD_SHARED_LIBS=OFF \
    -DGIT_EXECUTABLE=/usr/bin/true \
    -DGIT_EXE=/usr/bin/true \
    -DGGML_ACCELERATE=ON \
    -DGGML_BLAS=OFF \
    -DGGML_CCACHE=OFF \
    -DGGML_METAL=ON \
    -DGGML_METAL_EMBED_LIBRARY=ON \
    -DGGML_METAL_MACOSX_VERSION_MIN="$MIN_MACOS_VERSION" \
    -DGGML_METAL_NDEBUG=ON \
    -DGGML_NATIVE=OFF \
    -DGGML_OPENMP=OFF \
    -DWHISPER_BUILD_EXAMPLES=ON \
    -DWHISPER_BUILD_SERVER=ON \
    -DWHISPER_BUILD_TESTS=OFF
  SOURCE_DATE_EPOCH="$WHISPER_SOURCE_DATE_EPOCH" \
  "$CMAKE_BIN" --build "$whisper_build" --config Release \
    --target whisper-cli whisper-server --parallel "$jobs"

  rm -rf "$ARM_RUNTIME_DIR" "$VAD_DIR"
  mkdir -p "$ARM_RUNTIME_DIR" "$VAD_DIR"
  /usr/bin/install -m 0755 "$whisper_build/bin/whisper-cli" "$ARM_RUNTIME_DIR/whisper-cli"
  /usr/bin/install -m 0755 "$whisper_build/bin/whisper-server" "$ARM_RUNTIME_DIR/whisper-server"
  /usr/bin/install -m 0644 "$vad_model" "$VAD_DIR/silero_vad.onnx"

  validate_runtime "$RESOURCES_DIR"
  "$ARM_RUNTIME_DIR/whisper-cli" --help >/dev/null 2>&1
  "$ARM_RUNTIME_DIR/whisper-server" --help >/dev/null 2>&1
}

validate_runtime() {
  local root="$1"
  local arm_dir="$root/whisper-darwin-arm64"
  local vad="$root/vad/silero_vad.onnx"

  for binary in "$arm_dir/whisper-cli" "$arm_dir/whisper-server"; do
    validate_native_binary "$binary"
  done
  [[ -s "$vad" ]] || die "Silero VAD model is missing: $vad"
  assert_sha256 "$vad" "$SILERO_SHA256"
}

rust_gates() {
  cargo fmt --manifest-path "$TAURI_DIR/Cargo.toml" --all -- --check
  cargo clippy --locked --manifest-path "$TAURI_DIR/Cargo.toml" --all-targets -- -D warnings
  cargo test --locked --manifest-path "$TAURI_DIR/Cargo.toml" --lib
}

configure_signing() {
  SIGNING_LABEL="unsigned"
  unset APPLE_SIGNING_IDENTITY
  # Notarization is deliberately out of scope for this secret-free workflow.
  # Clear ambient credentials so a local signed run cannot be mislabeled.
  unset APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD
  unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID

  if [[ -n "${VOXFLOW_SIGNING_IDENTITY:-}" ]]; then
    /usr/bin/security find-identity -v -p codesigning \
      | grep -F -- "$VOXFLOW_SIGNING_IDENTITY" >/dev/null \
      || die "requested signing identity is not installed: $VOXFLOW_SIGNING_IDENTITY"
    export APPLE_SIGNING_IDENTITY="$VOXFLOW_SIGNING_IDENTITY"
    SIGNING_LABEL="signed-unnotarized"
  fi
}

validate_signing_state() {
  local app="$1"
  local details
  local team_identifier
  details="$(/usr/bin/codesign -dvvv "$app" 2>&1 || true)"
  if [[ "$SIGNING_LABEL" == "unsigned" ]]; then
    ! grep -q '^Authority=' <<< "$details" \
      || die "unsigned build unexpectedly contains a signing authority"
    team_identifier="$(awk -F= '$1 == "TeamIdentifier" { print $2 }' <<< "$details")"
    [[ -z "$team_identifier" || "$team_identifier" == "not set" ]] \
      || die "unsigned build unexpectedly contains TeamIdentifier=$team_identifier"
  else
    /usr/bin/codesign --verify --deep --strict "$app"
    team_identifier="$(awk -F= '$1 == "TeamIdentifier" { print $2 }' <<< "$details")"
    [[ -n "$team_identifier" && "$team_identifier" != "not set" ]] \
      || die "signed app has no TeamIdentifier"
    grep -q 'flags=.*runtime' <<< "$details" || die "signed app is missing hardened runtime"
  fi
}

validate_app_bundle() {
  local app="$1"
  local contents="$app/Contents"
  local bundled_resources="$contents/Resources/resources"
  local plist="$contents/Info.plist"
  local version="$2"
  local plist_version
  local plist_minimum
  local unexpected_runtime

  [[ -d "$app" ]] || die "app bundle is missing: $app"
  [[ -s "$contents/Resources/icon.icns" ]] || die "app icon is missing"
  validate_native_binary "$contents/MacOS/voxflow"
  validate_runtime "$bundled_resources"
  unexpected_runtime="$(find "$bundled_resources" -maxdepth 1 -type d \
    -name 'whisper-darwin-*' ! -name 'whisper-darwin-arm64' -print -quit)"
  [[ -z "$unexpected_runtime" ]] \
    || die "non-ARM64 whisper runtime was bundled: $unexpected_runtime"
  /usr/bin/cmp "$ARM_RUNTIME_DIR/whisper-cli" "$bundled_resources/whisper-darwin-arm64/whisper-cli"
  /usr/bin/cmp "$ARM_RUNTIME_DIR/whisper-server" "$bundled_resources/whisper-darwin-arm64/whisper-server"

  plist_version="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$plist")"
  plist_minimum="$(/usr/libexec/PlistBuddy -c 'Print :LSMinimumSystemVersion' "$plist")"
  [[ "$plist_version" == "$version" ]] || die "bundle version $plist_version does not match $version"
  [[ "$plist_minimum" == "$MIN_MACOS_VERSION" ]] \
    || die "bundle minimum macOS is $plist_minimum, expected $MIN_MACOS_VERSION"
  [[ "$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$plist")" == "com.nezeronxer.voxflow.macos" ]] \
    || die "unexpected bundle identifier"
  for key in NSMicrophoneUsageDescription NSInputMonitoringUsageDescription NSAppleEventsUsageDescription; do
    /usr/libexec/PlistBuddy -c "Print :$key" "$plist" >/dev/null \
      || die "Info.plist is missing $key"
  done
  validate_signing_state "$app"
}

build_bundles() {
  local version="$1"
  local tauri_cli="$APP_DIR/node_modules/@tauri-apps/cli/tauri.js"
  local attempt
  local built=0
  local dmg_candidates=()
  local source_dmg

  export MACOSX_DEPLOYMENT_TARGET="$MIN_MACOS_VERSION"
  for attempt in 1 2; do
    rm -rf "$TARGET_RELEASE_DIR/bundle/macos" "$TARGET_RELEASE_DIR/bundle/dmg"
    if [[ "$SIGNING_LABEL" == "unsigned" ]]; then
      if (cd "$APP_DIR" && node "$tauri_cli" build --ci --target "$TARGET_TRIPLE" \
        --bundles app,dmg --no-sign); then
        built=1
        break
      fi
    elif (cd "$APP_DIR" && node "$tauri_cli" build --ci --target "$TARGET_TRIPLE" \
      --bundles app,dmg); then
      built=1
      break
    fi
    [[ "$attempt" == 1 ]] || break
    echo "Tauri DMG packaging failed once; retrying after a clean bundle directory" >&2
    sleep 2
  done
  [[ "$built" == 1 ]] || die "Tauri app/DMG packaging failed twice"

  validate_app_bundle "$APP_BUNDLE" "$version"
  while IFS= read -r candidate; do
    dmg_candidates+=("$candidate")
  done < <(find "$TARGET_RELEASE_DIR/bundle/dmg" -maxdepth 1 -type f -name '*.dmg' -print)
  [[ "${#dmg_candidates[@]}" == 1 ]] \
    || die "expected exactly one DMG, found ${#dmg_candidates[@]}"
  source_dmg="${dmg_candidates[0]}"
  package_outputs "$source_dmg" "$version"
}

verify_dmg() {
  local dmg="$1"
  local version="$2"
  MOUNT_POINT="$TEMP_ROOT/dmg-mount"
  mkdir -p "$MOUNT_POINT"
  /usr/bin/hdiutil attach -readonly -nobrowse -mountpoint "$MOUNT_POINT" "$dmg" -quiet
  [[ -L "$MOUNT_POINT/Applications" ]] || die "DMG is missing the Applications alias"
  validate_app_bundle "$MOUNT_POINT/VoxFlow.app" "$version"
  /usr/bin/hdiutil detach "$MOUNT_POINT" -quiet
  rmdir "$MOUNT_POINT" 2>/dev/null || true
  MOUNT_POINT=""
}

verify_app_zip() {
  local archive="$1"
  local version="$2"
  local extract_dir="$TEMP_ROOT/app-zip"
  rm -rf "$extract_dir"
  mkdir -p "$extract_dir"
  /usr/bin/ditto -x -k "$archive" "$extract_dir"
  validate_app_bundle "$extract_dir/VoxFlow.app" "$version"
}

package_outputs() {
  local source_dmg="$1"
  local version="$2"
  local output_dir="$OUTPUT_ROOT/$version"
  local base="VoxFlow-macOS-${version}-arm64-${SIGNING_LABEL}"
  local output_dmg="$output_dir/$base.dmg"
  local output_zip="$output_dir/$base.app.zip"
  local checksums="$output_dir/SHA256SUMS.txt"
  local manifest="$output_dir/release-manifest.txt"
  local source_revision="${GITHUB_SHA:-unknown}"

  rm -rf "$output_dir"
  mkdir -p "$output_dir"
  /usr/bin/ditto "$source_dmg" "$output_dmg"
  /usr/bin/ditto -c -k --sequesterRsrc --keepParent "$APP_BUNDLE" "$output_zip"
  verify_dmg "$output_dmg" "$version"
  verify_app_zip "$output_zip" "$version"

  (
    cd "$output_dir"
    /usr/bin/shasum -a 256 "$(basename "$output_dmg")" "$(basename "$output_zip")" > "$checksums"
  )
  {
    echo "product=VoxFlow"
    echo "version=$version"
    echo "source_revision=$source_revision"
    echo "target=$TARGET_TRIPLE"
    echo "minimum_macos=$MIN_MACOS_VERSION"
    echo "signing=$SIGNING_LABEL"
    echo "notarized=false"
    echo "whisper_version=$WHISPER_VERSION"
    echo "whisper_commit=$WHISPER_COMMIT"
    echo "whisper_source_date_epoch=$WHISPER_SOURCE_DATE_EPOCH"
    echo "whisper_source_sha256=$WHISPER_ARCHIVE_SHA256"
    echo "whisper_macos11_patch_sha256=$WHISPER_MACOS11_PATCH_SHA256"
    echo "whisper_cli_sha256=$(sha256_of "$ARM_RUNTIME_DIR/whisper-cli")"
    echo "whisper_server_sha256=$(sha256_of "$ARM_RUNTIME_DIR/whisper-server")"
    echo "silero_commit=$SILERO_COMMIT"
    echo "silero_sha256=$SILERO_SHA256"
    echo "cmake_version=$CMAKE_VERSION"
    echo "package_lock_sha256=$(sha256_of "$APP_DIR/package-lock.json")"
    echo "cargo_lock_sha256=$(sha256_of "$TAURI_DIR/Cargo.lock")"
    echo "tauri_cli=$(cd "$APP_DIR" && node -p "require('./node_modules/@tauri-apps/cli/package.json').version")"
    echo "rustc=$(rustc --version)"
    echo "node=$(node --version)"
    echo "npm=$(npm --version)"
    echo "xcode=$(developer_tools_description)"
  } > "$manifest"

  (cd "$output_dir" && /usr/bin/shasum -a 256 -c "$(basename "$checksums")")

  echo "Release artifacts: $output_dir"
  /bin/cat "$checksums"
}

main() {
  assert_runner
  TEMP_ROOT="$(/usr/bin/mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/voxflow-macos-arm64.XXXXXX")"
  if [[ "$RUNTIME_ONLY" == 1 ]]; then
    prepare_runtime
    echo "Verified macOS ARM64 runtime: $ARM_RUNTIME_DIR"
    return
  fi
  require_command node
  require_command npm
  version_gate
  frontend_gates
  assert_tauri_cli

  prepare_runtime
  rust_gates
  configure_signing

  local version
  version="$(cd "$APP_DIR" && node -p "require('./package.json').version")"
  build_bundles "$version"
}

main
