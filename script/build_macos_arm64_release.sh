#!/usr/bin/env bash
set -euo pipefail

# Reproducible-input macOS release builder for VoxFlow.
#
# The secret-free path uses Tauri's complete ad-hoc bundle signing (`-`). This
# is not a trusted Developer ID signature, but it binds Info.plist/resources and
# prevents the linker-only signature from becoming an invalid distributed app.
# A local machine or CI runner may opt into Developer ID signing by setting
# VOXFLOW_SIGNING_IDENTITY to an identity already installed in the current
# keychain. When either complete Apple ID or App Store Connect API credentials
# are also present, Tauri notarizes/staples the app and this builder does the
# same for the final DMG. Certificate import remains the caller's responsibility
# so this script never writes secret material to the repository or a persistent
# keychain.

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
MACOS_TAURI_CONFIG="$TAURI_DIR/tauri.macos.conf.json"
RESOURCES_DIR="$TAURI_DIR/resources"
ARM_RUNTIME_DIR="$RESOURCES_DIR/whisper-darwin-arm64"
VAD_DIR="$RESOURCES_DIR/vad"
TARGET_RELEASE_DIR="$TAURI_DIR/target/$TARGET_TRIPLE/release"
APP_BUNDLE="$TARGET_RELEASE_DIR/bundle/macos/VoxFlow.app"
OUTPUT_ROOT="${VOXFLOW_RELEASE_DIR:-$APP_DIR/release/macos}"
RELEASE_TAG=""
RUNTIME_ONLY=0
SIGNING_LABEL=""
NOTARIZATION_AUTH="none"
NOTARIZED="false"

TEMP_ROOT=""
MOUNT_POINT=""

usage() {
  cat >&2 <<'EOF'
Usage: script/build_macos_arm64_release.sh [--tag vX.Y.Z] [--runtime-only]

Environment:
  VOXFLOW_RELEASE_DIR         Output parent (default: voxflow/release/macos)
  VOXFLOW_SIGNING_IDENTITY    Optional installed Developer ID identity.
                              Omit for the CI-default complete ad-hoc seal.
  APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID
                              Optional complete Apple ID notarization tuple.
  APPLE_API_ISSUER, APPLE_API_KEY, APPLE_API_KEY_PATH
                              Optional complete App Store Connect API tuple.
                              Never configure both notarization methods.
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
  local apple_id_values=0
  local api_values=0
  local variable
  local identity_listing

  SIGNING_LABEL="adhoc"
  NOTARIZATION_AUTH="none"
  NOTARIZED="false"
  export APPLE_SIGNING_IDENTITY="-"
  # The release builder consumes only an identity already imported into a
  # keychain. Avoid forwarding certificate material to child processes.
  unset APPLE_CERTIFICATE APPLE_CERTIFICATE_PASSWORD

  for variable in APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID; do
    if [[ -n "${!variable:-}" ]]; then
      apple_id_values=$((apple_id_values + 1))
    fi
  done
  for variable in APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH; do
    if [[ -n "${!variable:-}" ]]; then
      api_values=$((api_values + 1))
    fi
  done

  if [[ "$apple_id_values" -gt 0 && "$apple_id_values" -lt 3 ]]; then
    die "partial Apple ID notarization credentials; set APPLE_ID, APPLE_PASSWORD, and APPLE_TEAM_ID together"
  fi
  if [[ "$api_values" -gt 0 && "$api_values" -lt 3 ]]; then
    die "partial App Store Connect notarization credentials; set APPLE_API_ISSUER, APPLE_API_KEY, and APPLE_API_KEY_PATH together"
  fi
  if [[ "$apple_id_values" == 3 && "$api_values" == 3 ]]; then
    die "both notarization authentication methods are configured; choose exactly one"
  fi

  if [[ -z "${VOXFLOW_SIGNING_IDENTITY:-}" || "$VOXFLOW_SIGNING_IDENTITY" == "-" ]]; then
    if [[ "$apple_id_values" -gt 0 || "$api_values" -gt 0 ]]; then
      die "notarization credentials require an installed Developer ID Application identity"
    fi
    # Ambient credentials must never turn a secret-free build into a mislabeled
    # or partially notarized artifact.
    unset APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH
    unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
    return
  fi

  identity_listing="$(/usr/bin/security find-identity -v -p codesigning)"
  grep -F -- "$VOXFLOW_SIGNING_IDENTITY" <<< "$identity_listing" >/dev/null \
    || die "requested signing identity is not installed: $VOXFLOW_SIGNING_IDENTITY"
  export APPLE_SIGNING_IDENTITY="$VOXFLOW_SIGNING_IDENTITY"
  SIGNING_LABEL="signed-unnotarized"

  if [[ "$apple_id_values" == 3 ]]; then
    NOTARIZATION_AUTH="apple-id"
    NOTARIZED="true"
    SIGNING_LABEL="developer-id-notarized"
    unset APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH
  elif [[ "$api_values" == 3 ]]; then
    [[ -f "$APPLE_API_KEY_PATH" && -r "$APPLE_API_KEY_PATH" ]] \
      || die "APPLE_API_KEY_PATH is not a readable private key file"
    NOTARIZATION_AUTH="app-store-connect-api"
    NOTARIZED="true"
    SIGNING_LABEL="developer-id-notarized"
    unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
  else
    unset APPLE_API_ISSUER APPLE_API_KEY APPLE_API_KEY_PATH
    unset APPLE_ID APPLE_PASSWORD APPLE_TEAM_ID
  fi

  if [[ "$NOTARIZED" == "true" ]]; then
    grep -F -- "$VOXFLOW_SIGNING_IDENTITY" <<< "$identity_listing" \
      | grep -F 'Developer ID Application:' >/dev/null \
      || die "notarized distribution requires a Developer ID Application identity"
  fi
}

validate_macos_bundle_config() {
  [[ -s "$MACOS_TAURI_CONFIG" ]] \
    || die "macOS Tauri config is missing: $MACOS_TAURI_CONFIG"
  python3 - "$MACOS_TAURI_CONFIG" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as stream:
    config = json.load(stream)

bundle = config.get("bundle", {})
targets = set(bundle.get("targets", []))
resources = set(bundle.get("resources", []))
expected_targets = {"app", "dmg"}
expected_resources = {
    "resources/whisper-darwin-arm64/*",
    "resources/vad/*",
}

if targets != expected_targets:
    raise SystemExit(
        f"macOS bundle targets must be exactly {sorted(expected_targets)}, "
        f"got {sorted(targets)}"
    )
if resources != expected_resources:
    raise SystemExit(
        f"macOS bundle resources must be exactly {sorted(expected_resources)}, "
        f"got {sorted(resources)}"
    )
PY
}

sign_runtime_binaries() {
  local identity="$APPLE_SIGNING_IDENTITY"
  local timestamp_args=(--timestamp)
  local binary identifier details
  if [[ "$identity" == "-" ]]; then
    timestamp_args=(--timestamp=none)
  fi

  while IFS='|' read -r binary identifier; do
    /usr/bin/codesign --force --sign "$identity" \
      --identifier "$identifier" --options runtime "${timestamp_args[@]}" "$binary"
    /usr/bin/codesign --verify --strict --verbose=4 "$binary"
    details="$(/usr/bin/codesign -dvvv "$binary" 2>&1)"
    grep -q 'flags=.*runtime' <<< "$details" \
      || die "runtime sidecar is missing hardened runtime: $binary"
    ! grep -q 'linker-signed' <<< "$details" \
      || die "runtime sidecar still has only a linker signature: $binary"
    if [[ "$SIGNING_LABEL" == "adhoc" ]]; then
      grep -q '^Signature=adhoc$' <<< "$details" \
        || die "runtime sidecar is not ad-hoc signed: $binary"
    fi
  done <<EOF
$ARM_RUNTIME_DIR/whisper-cli|com.nezeronxer.voxflow.whisper-cli
$ARM_RUNTIME_DIR/whisper-server|com.nezeronxer.voxflow.whisper-server
EOF
}

validate_signing_state() {
  local app="$1"
  local details
  local embedded_entitlements="$TEMP_ROOT/embedded-entitlements.plist"
  local team_identifier
  local nested_details
  local nested_binary
  details="$(/usr/bin/codesign -dvvv "$app" 2>&1)" \
    || die "app bundle has no complete code signature: $app"
  /usr/bin/codesign --verify --strict --verbose=4 "$app"
  /usr/bin/codesign --verify --deep --strict --verbose=4 "$app"
  [[ -s "$app/Contents/_CodeSignature/CodeResources" ]] \
    || die "app bundle has no sealed CodeResources"
  grep -q '^Info.plist entries=' <<< "$details" \
    || die "app signature does not bind Info.plist"
  grep -q '^Sealed Resources version=2' <<< "$details" \
    || die "app signature does not seal bundle resources"
  grep -q 'flags=.*runtime' <<< "$details" || die "app is missing hardened runtime"
  /usr/bin/codesign --display --entitlements - --xml "$app" \
    > "$embedded_entitlements" 2>/dev/null \
    || die "could not extract embedded app entitlements: $app"
  [[ -s "$embedded_entitlements" ]] \
    || die "app signature has no embedded entitlements: $app"
  python3 - "$embedded_entitlements" <<'PY'
import plistlib
import sys

path = sys.argv[1]
with open(path, "rb") as stream:
    entitlements = plistlib.load(stream)

required = (
    "com.apple.security.device.audio-input",
    "com.apple.security.automation.apple-events",
)
missing = [key for key in required if entitlements.get(key) is not True]
if missing:
    raise SystemExit(
        "missing required true app entitlements: " + ", ".join(missing)
    )
PY

  if [[ "$SIGNING_LABEL" == "adhoc" ]]; then
    ! grep -q '^Authority=' <<< "$details" \
      || die "ad-hoc build unexpectedly contains a signing authority"
    team_identifier="$(awk -F= '$1 == "TeamIdentifier" { print $2 }' <<< "$details")"
    [[ -z "$team_identifier" || "$team_identifier" == "not set" ]] \
      || die "ad-hoc build unexpectedly contains TeamIdentifier=$team_identifier"
    grep -q '^Signature=adhoc$' <<< "$details" || die "app is not ad-hoc signed"
  else
    team_identifier="$(awk -F= '$1 == "TeamIdentifier" { print $2 }' <<< "$details")"
    [[ -n "$team_identifier" && "$team_identifier" != "not set" ]] \
      || die "signed app has no TeamIdentifier"
  fi

  for nested_binary in \
    "$app/Contents/Resources/resources/whisper-darwin-arm64/whisper-cli" \
    "$app/Contents/Resources/resources/whisper-darwin-arm64/whisper-server"; do
    /usr/bin/codesign --verify --strict --verbose=4 "$nested_binary"
    nested_details="$(/usr/bin/codesign -dvvv "$nested_binary" 2>&1)"
    grep -q 'flags=.*runtime' <<< "$nested_details" \
      || die "bundled runtime sidecar is missing hardened runtime: $nested_binary"
    ! grep -q 'linker-signed' <<< "$nested_details" \
      || die "bundled runtime sidecar has only a linker signature: $nested_binary"
    if [[ "$SIGNING_LABEL" == "adhoc" ]]; then
      grep -q '^Signature=adhoc$' <<< "$nested_details" \
        || die "bundled runtime sidecar is not ad-hoc signed: $nested_binary"
    fi
  done

  # syspolicy_check still reports the expected Adhoc Signed App / missing
  # notarization ticket in the secret-free build. A structural Codesign Error,
  # however, is always a broken artifact and previously produced macOS's
  # misleading "application is damaged" dialog after a browser download.
  if [[ -x /usr/bin/syspolicy_check ]]; then
    local policy_report
    policy_report="$(/usr/bin/syspolicy_check distribution "$app" 2>&1 || true)"
    ! grep -q 'Codesign Error' <<< "$policy_report" \
      || die "Gatekeeper found a structural code-signing error: $policy_report"
  fi
}

validate_notarization_state() {
  local app="$1"
  local dmg="$2"

  [[ "$NOTARIZED" == "true" ]] || return
  /usr/bin/xcrun stapler validate "$app"
  /usr/bin/xcrun stapler validate "$dmg"
  /usr/sbin/spctl --assess --verbose=4 --type execute "$app"
  /usr/sbin/spctl --assess --verbose=4 --type open \
    --context context:primary-signature "$dmg"
}

notarize_dmg() {
  local dmg="$1"
  local response="$TEMP_ROOT/notarytool-dmg-response.json"
  local log_file="$TEMP_ROOT/notarytool-dmg-log.json"
  local submission_id
  local status
  local -a auth_args

  [[ "$NOTARIZED" == "true" ]] || return
  case "$NOTARIZATION_AUTH" in
    apple-id)
      auth_args=(
        --apple-id "$APPLE_ID"
        --password "$APPLE_PASSWORD"
        --team-id "$APPLE_TEAM_ID"
      )
      ;;
    app-store-connect-api)
      auth_args=(
        --key "$APPLE_API_KEY_PATH"
        --key-id "$APPLE_API_KEY"
        --issuer "$APPLE_API_ISSUER"
      )
      ;;
    *)
      die "missing notarization authentication mode for DMG"
      ;;
  esac

  # Tauri 2.11 notarizes and staples the app bundle, then signs the generated
  # DMG. Submit the final disk image separately so both distributed formats
  # carry an offline-verifiable Apple ticket.
  /usr/bin/codesign --verify --strict --verbose=4 "$dmg"
  if ! /usr/bin/xcrun notarytool submit "$dmg" \
    "${auth_args[@]}" --wait --output-format json > "$response"; then
    /bin/cat "$response" >&2 || true
    die "DMG notarization submission failed"
  fi

  submission_id="$(python3 - "$response" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    print(json.load(stream).get("id", ""))
PY
)"
  status="$(python3 - "$response" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    print(json.load(stream).get("status", ""))
PY
)"
  [[ -n "$submission_id" ]] || die "notarytool returned no DMG submission id"

  # Apple recommends reviewing the log even for accepted submissions. Keep it
  # in the ephemeral build directory for diagnostics without publishing it.
  /usr/bin/xcrun notarytool log "$submission_id" \
    "${auth_args[@]}" "$log_file" || true
  [[ "$status" == "Accepted" ]] || {
    /bin/cat "$log_file" >&2 || true
    die "DMG notarization status is $status"
  }

  /usr/bin/xcrun stapler staple "$dmg"
  /usr/bin/xcrun stapler validate "$dmg"
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
  for unexpected_runtime in whisper whisper-cuda; do
    [[ ! -e "$bundled_resources/$unexpected_runtime" ]] \
      || die "Windows whisper runtime polluted the macOS bundle: $bundled_resources/$unexpected_runtime"
  done
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
    if (cd "$APP_DIR" && node "$tauri_cli" build --ci --target "$TARGET_TRIPLE" \
      --bundles app,dmg --config "$MACOS_TAURI_CONFIG"); then
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
  notarize_dmg "$source_dmg"
  validate_notarization_state "$APP_BUNDLE" "$source_dmg"
  package_outputs "$source_dmg" "$version"
}

verify_dmg() {
  local dmg="$1"
  local version="$2"
  if [[ "$NOTARIZED" == "true" ]]; then
    /usr/bin/xcrun stapler validate "$dmg"
    /usr/sbin/spctl --assess --verbose=4 --type open \
      --context context:primary-signature "$dmg"
  fi
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
  if [[ "$NOTARIZED" == "true" ]]; then
    /usr/bin/xcrun stapler validate "$extract_dir/VoxFlow.app"
    /usr/sbin/spctl --assess --verbose=4 --type execute \
      "$extract_dir/VoxFlow.app"
  fi
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
    echo "notarized=$NOTARIZED"
    echo "notarization_auth=$NOTARIZATION_AUTH"
    echo "microphone_entitlement=true"
    echo "apple_events_entitlement=true"
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
    echo "tauri_macos_config_sha256=$(sha256_of "$MACOS_TAURI_CONFIG")"
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
  validate_macos_bundle_config
  frontend_gates
  assert_tauri_cli

  prepare_runtime
  rust_gates
  configure_signing
  sign_runtime_binaries

  local version
  version="$(cd "$APP_DIR" && node -p "require('./package.json').version")"
  build_bundles "$version"
}

main
