#!/usr/bin/env bash
# Linux x86_64: собирает рантайм, который лежит в bundle.resources
# (voxflow/src-tauri/tauri.linux.conf.json):
#   resources/whisper-linux-x64/{whisper-cli,whisper-server}  - whisper.cpp, CPU, статический
#   resources/vad/silero_vad.onnx                              - Silero VAD
# Версии и SHA-256 те же, что в script/build_macos_arm64_release.sh.
# ONNX Runtime (GigaAM/Parakeet/VAD) подтягивает крейт ort и линкует статически,
# отдельно его класть в пакет не нужно.
#
# Нужны: bash, curl, tar, sha256sum, cmake, g++, make.
set -euo pipefail

readonly WHISPER_COMMIT="23ee03506a91ac3d3f0071b40e66a430eebdfa1d"
readonly WHISPER_ARCHIVE_SHA256="c8b0de473e9ec47a74bdf6104425c709261beeada8d6d7c1fec7432be701d032"
readonly WHISPER_ARCHIVE_URL="https://codeload.github.com/ggml-org/whisper.cpp/tar.gz/${WHISPER_COMMIT}"
readonly SILERO_COMMIT="b163605b3f44c3aadf28f97b125a2f7c461e9a7f"
readonly SILERO_SHA256="1a153a22f4509e292a94e67d6f9b85e8deb25b4988682b7e174c65279d8788e3"
readonly SILERO_URL="https://raw.githubusercontent.com/snakers4/silero-vad/${SILERO_COMMIT}/src/silero_vad/data/silero_vad.onnx"

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RESOURCES_DIR="$ROOT_DIR/voxflow/src-tauri/resources"
RUNTIME_DIR="$RESOURCES_DIR/whisper-linux-x64"
VAD_DIR="$RESOURCES_DIR/vad"

die() { echo "error: $*" >&2; exit 1; }

[[ "$(uname -s)" == "Linux" && "$(uname -m)" == "x86_64" ]] || die "только Linux x86_64"

TEMP_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEMP_ROOT"' EXIT

download_verified() { # url sha256 dest
  curl --fail --silent --show-error --location --retry 3 -o "$3" "$1"
  echo "$2  $3" | sha256sum --check --status || die "SHA-256 не совпал: $1"
}

download_verified "$WHISPER_ARCHIVE_URL" "$WHISPER_ARCHIVE_SHA256" "$TEMP_ROOT/whisper.tar.gz"
download_verified "$SILERO_URL" "$SILERO_SHA256" "$TEMP_ROOT/silero_vad.onnx"

mkdir -p "$TEMP_ROOT/src"
tar -xzf "$TEMP_ROOT/whisper.tar.gz" -C "$TEMP_ROOT/src" --strip-components=1
[[ -s "$TEMP_ROOT/src/CMakeLists.txt" ]] || die "исходники whisper.cpp не распаковались"

# Статический ggml, без libgomp; libstdc++/libgcc внутрь, чтобы бинарь шёл
# на любом дистрибутиве. GGML_NATIVE=OFF: сборка не привязана к CPU раннера
# (в итоге нужен AVX2 - у любого x86-64 примерно с 2015 года).
cmake -S "$TEMP_ROOT/src" -B "$TEMP_ROOT/build" \
  -DCMAKE_BUILD_TYPE=Release \
  -DBUILD_SHARED_LIBS=OFF \
  -DGGML_NATIVE=OFF \
  -DGGML_OPENMP=OFF \
  -DGGML_CCACHE=OFF \
  -DGIT_EXECUTABLE=/bin/true \
  -DWHISPER_BUILD_EXAMPLES=ON \
  -DWHISPER_BUILD_SERVER=ON \
  -DWHISPER_BUILD_TESTS=OFF \
  -DCMAKE_EXE_LINKER_FLAGS="-static-libstdc++ -static-libgcc"
cmake --build "$TEMP_ROOT/build" --config Release \
  --target whisper-cli whisper-server --parallel "$(nproc)"

rm -rf "$RUNTIME_DIR" "$VAD_DIR"
mkdir -p "$RUNTIME_DIR" "$VAD_DIR"
install -m 0755 "$TEMP_ROOT/build/bin/whisper-cli" "$RUNTIME_DIR/whisper-cli"
install -m 0755 "$TEMP_ROOT/build/bin/whisper-server" "$RUNTIME_DIR/whisper-server"
install -m 0644 "$TEMP_ROOT/silero_vad.onnx" "$VAD_DIR/silero_vad.onnx"

"$RUNTIME_DIR/whisper-cli" --help >/dev/null 2>&1
"$RUNTIME_DIR/whisper-server" --help >/dev/null 2>&1
echo "Linux runtime готов: $RUNTIME_DIR, $VAD_DIR"
