#!/usr/bin/env bash
set -euo pipefail

MODE="${1:-run}"
APP_NAME="VoxFlow"
PROCESS_NAME="voxflow"
BUNDLE_ID="com.nezeronxer.voxflow.macos"
TARGET_TRIPLE="aarch64-apple-darwin"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT_DIR/voxflow"
TAURI_DIR="$APP_DIR/src-tauri"
TAURI_CLI="$APP_DIR/node_modules/@tauri-apps/cli/tauri.js"
BUILD_APP="$TAURI_DIR/target/$TARGET_TRIPLE/release/bundle/macos/$APP_NAME.app"
INSTALLED_APP="/Applications/$APP_NAME.app"

NODE_BIN="${VOXFLOW_NODE_BIN:-$(command -v node 2>/dev/null || true)}"
[[ -x "$NODE_BIN" ]] || {
  echo "Node.js is required to build VoxFlow (set VOXFLOW_NODE_BIN to override)" >&2
  exit 1
}
CURRENT_VERSION="$("$NODE_BIN" -p "require('$APP_DIR/package.json').version")"

STAGE_ROOT=""
BACKUP_APP=""

cleanup_install_transaction() {
  if [[ -n "$STAGE_ROOT" && -d "$STAGE_ROOT" ]]; then
    rm -rf -- "$STAGE_ROOT"
  fi
  if [[ -n "$BACKUP_APP" && -d "$BACKUP_APP" && ! -d "$INSTALLED_APP" ]]; then
    mv -- "$BACKUP_APP" "$INSTALLED_APP"
  fi
}
trap cleanup_install_transaction EXIT

usage() {
  echo "usage: $0 [run|--debug|--logs|--telemetry|--verify]" >&2
}

stop_running_app() {
  pkill -x "$PROCESS_NAME" >/dev/null 2>&1 || true
  for _ in {1..30}; do
    pgrep -x "$PROCESS_NAME" >/dev/null 2>&1 || return 0
    sleep 0.1
  done
  echo "$APP_NAME did not stop in time" >&2
  return 1
}

build_app() {
  if [[ ! -x "$TAURI_CLI" ]]; then
    local npm_bin
    npm_bin="$(command -v npm 2>/dev/null || true)"
    [[ -x "$npm_bin" ]] || {
      echo "Tauri dependencies are missing and npm is unavailable" >&2
      return 1
    }
    (cd "$APP_DIR" && "$npm_bin" ci)
  fi
  (
    cd "$APP_DIR"
    export PATH="$(dirname "$NODE_BIN"):$PATH"
    MACOSX_DEPLOYMENT_TARGET=11.0 \
      APPLE_SIGNING_IDENTITY="${VOXFLOW_SIGNING_IDENTITY:--}" \
      "$NODE_BIN" "$TAURI_CLI" build --ci --target "$TARGET_TRIPLE" --bundles app
  )
  [[ -d "$BUILD_APP" ]] || {
    echo "built app bundle is missing: $BUILD_APP" >&2
    return 1
  }
  codesign --verify --deep --strict --verbose=2 "$BUILD_APP"
}

install_app_atomically() {
  STAGE_ROOT="$(mktemp -d "/Applications/.voxflow-stage.XXXXXX")"
  local staged_app="$STAGE_ROOT/$APP_NAME.app"
  /usr/bin/ditto "$BUILD_APP" "$staged_app"
  codesign --verify --deep --strict --verbose=2 "$staged_app"

  BACKUP_APP="/Applications/.VoxFlow-previous-$$.app"
  rm -rf -- "$BACKUP_APP"
  if [[ -d "$INSTALLED_APP" ]]; then
    mv -- "$INSTALLED_APP" "$BACKUP_APP"
  fi
  if ! mv -- "$staged_app" "$INSTALLED_APP"; then
    if [[ -d "$BACKUP_APP" ]]; then
      mv -- "$BACKUP_APP" "$INSTALLED_APP"
    fi
    BACKUP_APP=""
    return 1
  fi
  if ! codesign --verify --deep --strict --verbose=2 "$INSTALLED_APP"; then
    rm -rf -- "$INSTALLED_APP"
    if [[ -d "$BACKUP_APP" ]]; then
      mv -- "$BACKUP_APP" "$INSTALLED_APP"
    fi
    BACKUP_APP=""
    return 1
  fi
  rm -rf -- "$BACKUP_APP"
  BACKUP_APP=""
  rm -rf -- "$STAGE_ROOT"
  STAGE_ROOT=""
}

version_is_older() {
  python3 - "$1" "$2" <<'PY'
import sys

def parse(value: str) -> tuple[int, int, int]:
    parts = value.split(".")
    if len(parts) != 3 or not all(part.isdigit() for part in parts):
        raise SystemExit(2)
    return tuple(map(int, parts))

raise SystemExit(0 if parse(sys.argv[1]) < parse(sys.argv[2]) else 1)
PY
}

cleanup_old_local_versions() {
  local release_root="$APP_DIR/release/macos"
  local candidate name
  if [[ -d "$release_root" ]]; then
    for candidate in "$release_root"/*; do
      [[ -d "$candidate" ]] || continue
      name="$(basename "$candidate")"
      if [[ "$name" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] && version_is_older "$name" "$CURRENT_VERSION"; then
        rm -rf -- "$candidate"
      fi
    done
  fi
}

open_app() {
  /usr/bin/open -n "$INSTALLED_APP"
}

verify_process() {
  for _ in {1..50}; do
    if pgrep -x "$PROCESS_NAME" >/dev/null 2>&1; then
      return 0
    fi
    sleep 0.1
  done
  echo "$APP_NAME did not start" >&2
  return 1
}

case "$MODE" in
  run|--debug|debug|--logs|logs|--telemetry|telemetry|--verify|verify) ;;
  *)
    usage
    exit 2
    ;;
esac

stop_running_app
build_app
install_app_atomically

case "$MODE" in
  --debug|debug)
    cleanup_old_local_versions
    lldb -- "$INSTALLED_APP/Contents/MacOS/$PROCESS_NAME"
    ;;
  --logs|logs)
    open_app
    verify_process
    cleanup_old_local_versions
    /usr/bin/log stream --info --style compact --predicate "process == \"$PROCESS_NAME\""
    ;;
  --telemetry|telemetry)
    open_app
    verify_process
    cleanup_old_local_versions
    /usr/bin/log stream --info --style compact --predicate "subsystem == \"$BUNDLE_ID\" OR process == \"$PROCESS_NAME\""
    ;;
  run|--verify|verify)
    open_app
    verify_process
    cleanup_old_local_versions
    ;;
esac
