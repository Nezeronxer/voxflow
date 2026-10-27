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

readonly SILERO_COMMIT="b163605b3f44c3aadf28f97b125a2f7c461e9a7f"
readonly SILERO_SHA256="1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
readonly SILERO_URL="https://raw.githubusercontent.com/snakers4/silero-vad/${SILERO_COMMIT}/src/silero_vad/data/silero_vad.onnx"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT_DIR/voxflow"
TAURI_DIR="$APP_DIR/src-tauri"
MACOS_TAURI_CONFIG="$TAURI_DIR/tauri.macos.conf.json"
RESOURCES_DIR="$TAURI_DIR/resources"
VAD_DIR="$RESOURCES_DIR/vad"
TARGET_RELEASE_DIR="$TAURI_DIR/target/$TARGET_TRIPLE/release"
APP_BUNDLE="$TARGET_RELEASE_DIR/bundle/macos/VoxFlow.app"
OUTPUT_ROOT="${VOXFLOW_RELEASE_DIR:-$APP_DIR/release/macos}"
RELEASE_TAG=""
RUNTIME_ONLY=0
SIGNING_LABEL=""
SELF_SIGNED="0"
NOTARIZATION_AUTH="none"
NOTARIZED="false"

TEMP_ROOT=""
MOUNT_POINT=""

usage() {
  cat >&2 <<'EOF'
Usage: script/build_macos_arm64_release.sh [--tag vX.Y.Z] [--runtime-only]

Environment:
  VOXFLOW_RELEASE_DIR         Output parent (default: voxflow/release/macos)
  VOXFLOW_SIGNING_IDENTITY    Optional installed identity: Developer ID Application
                              (notarized with the tuples below) or a self-signed
                              code-signing certificate (signed-unnotarized assets).
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

# Рантайм бандла - только Silero VAD: Whisper убран (D-065), ONNX Runtime
# линкуется в бинарник крейтом ort.
prepare_runtime() {
  local vad_model="$TEMP_ROOT/silero_vad.onnx"

  download_verified "$SILERO_URL" "$SILERO_SHA256" "$vad_model"
  rm -rf "$VAD_DIR"
  mkdir -p "$VAD_DIR"
  /usr/bin/install -m 0644 "$vad_model" "$VAD_DIR/silero_vad.onnx"
  validate_runtime "$RESOURCES_DIR"
}

validate_runtime() {
  local root="$1"
  local vad="$root/vad/silero_vad.onnx"

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
  if ! grep -F -- "$VOXFLOW_SIGNING_IDENTITY" <<< "$identity_listing" >/dev/null; then
    # An untrusted self-signed certificate is hidden by -v; look in the full list,
    # but a Developer ID identity must be valid, so it never passes this way.
    identity_listing="$(/usr/bin/security find-identity -p codesigning)"
    grep -F -- "$VOXFLOW_SIGNING_IDENTITY" <<< "$identity_listing" >/dev/null \
      || die "requested signing identity is not installed: $VOXFLOW_SIGNING_IDENTITY"
    ! grep -F -- "$VOXFLOW_SIGNING_IDENTITY" <<< "$identity_listing" \
      | grep -F 'Developer ID Application:' >/dev/null \
      || die "Developer ID identity is not valid (expired, revoked or untrusted): $VOXFLOW_SIGNING_IDENTITY"
  fi
  # Self-signed is a property of the identity itself, whether or not it is trusted.
  if ! grep -F -- "$VOXFLOW_SIGNING_IDENTITY" <<< "$identity_listing" \
    | grep -F 'Developer ID Application:' >/dev/null; then
    SELF_SIGNED="1"
  fi
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

validate_signing_state() {
  local app="$1"
  local details
  local embedded_entitlements="$TEMP_ROOT/embedded-entitlements.plist"
  local team_identifier
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
  elif [[ "$SELF_SIGNED" == "1" ]]; then
    # Self-signed: no Apple team, but a stable certificate leaf keeps the
    # designated requirement (and the user's TCC grants) the same across builds.
    grep -q '^Authority=' <<< "$details" || die "self-signed app has no signing authority"
  else
    team_identifier="$(awk -F= '$1 == "TeamIdentifier" { print $2 }' <<< "$details")"
    [[ -n "$team_identifier" && "$team_identifier" != "not set" ]] \
      || die "signed app has no TeamIdentifier"
  fi

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

  if [[ "$NOTARIZED" != "true" ]]; then
    echo "Skipping notarization-state validation for signing=$SIGNING_LABEL"
    return 0
  fi
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

  if [[ "$NOTARIZED" != "true" ]]; then
    echo "Skipping DMG notarization for signing=$SIGNING_LABEL"
    return 0
  fi
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
  unexpected_runtime="$(find "$bundled_resources" -maxdepth 1 -name 'whisper*' -print -quit)"
  [[ -z "$unexpected_runtime" ]] \
    || die "whisper runtime must not be bundled (D-065): $unexpected_runtime"

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

add_dmg_help_file() {
  local dmg="$1"
  local help_file="$ROOT_DIR/script/dmg/Если macOS ругается.txt"
  local rw="$TEMP_ROOT/dmg-rw.dmg"
  local out="$TEMP_ROOT/dmg-help.dmg"
  local mount="$TEMP_ROOT/dmg-help-mount"

  [[ -f "$help_file" ]] || die "missing $help_file"
  rm -f "$rw" "$out"
  /usr/bin/hdiutil convert "$dmg" -format UDRW -o "$rw" -quiet
  mkdir -p "$mount"
  MOUNT_POINT="$mount"
  /usr/bin/hdiutil attach -nobrowse -mountpoint "$mount" "$rw" -quiet
  /usr/bin/ditto "$help_file" "$mount/Если macOS ругается.txt"
  local detached=0 try
  for try in 1 2 3 4 5; do
    if /usr/bin/hdiutil detach "$mount" -quiet; then detached=1; break; fi
    sleep 2
  done
  if [[ "$detached" != 1 ]]; then
    /usr/bin/hdiutil detach "$mount" -force -quiet \
      || die "cannot detach $mount after writing the help file into the DMG"
  fi
  MOUNT_POINT=""
  /usr/bin/hdiutil convert "$rw" -format UDZO -o "$out" -quiet
  # Содержимое образа изменилось: подпись Tauri больше не действует, подписываем заново.
  # Метка времени Apple нужна только Developer ID (иначе нотаризация образа не пройдёт).
  local timestamp_arg=--timestamp
  if [[ "$APPLE_SIGNING_IDENTITY" == "-" || "$SELF_SIGNED" == "1" ]]; then
    timestamp_arg=--timestamp=none
  fi
  /usr/bin/codesign --force --sign "$APPLE_SIGNING_IDENTITY" "$timestamp_arg" "$out"
  mv -f "$out" "$dmg"
  rm -f "$rw"
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
  add_dmg_help_file "$source_dmg"
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
  [[ -f "$MOUNT_POINT/Если macOS ругается.txt" ]] || die "DMG is missing the help file"
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
    echo "silero_commit=$SILERO_COMMIT"
    echo "silero_sha256=$SILERO_SHA256"
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
    echo "Verified macOS ARM64 runtime: $VAD_DIR"
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

  local version
  version="$(cd "$APP_DIR" && node -p "require('./package.json').version")"
  build_bundles "$version"
}

main
