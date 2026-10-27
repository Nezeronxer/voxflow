#!/usr/bin/env bash
# Linux x86_64: собирает рантайм, который лежит в bundle.resources
# (voxflow/src-tauri/tauri.linux.conf.json):
#   resources/vad/silero_vad.onnx - Silero VAD
# ONNX Runtime (GigaAM/Parakeet/VAD) подтягивает крейт ort и линкует статически,
# отдельно его класть в пакет не нужно. Whisper убран (D-065).
#
# Нужны: bash, curl, sha256sum.
set -euo pipefail

readonly SILERO_COMMIT="b163605b3f44c3aadf28f97b125a2f7c461e9a7f"
readonly SILERO_SHA256="1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
readonly SILERO_URL="https://raw.githubusercontent.com/snakers4/silero-vad/${SILERO_COMMIT}/src/silero_vad/data/silero_vad.onnx"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESOURCES_DIR="$ROOT_DIR/voxflow/src-tauri/resources"
VAD_DIR="$RESOURCES_DIR/vad"

die() { echo "error: $*" >&2; exit 1; }

[[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" ]] || die "только Linux x86_64"

TEMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEMP_ROOT"' EXIT

download_verified() { # url sha256 dest
  curl --fail --silent --show-error --location --retry 3 -o "$3" "$1"
  echo "$2  $3" | sha256sum --check --status || die "SHA-256 не совпал: $1"
}

download_verified "$SILERO_URL" "$SILERO_SHA256" "$TEMP_ROOT/silero_vad.onnx"

rm -rf "$VAD_DIR"
mkdir -p "$VAD_DIR"
install -m 0644 "$TEMP_ROOT/silero_vad.onnx" "$VAD_DIR/silero_vad.onnx"
echo "Linux runtime готов: $VAD_DIR"
