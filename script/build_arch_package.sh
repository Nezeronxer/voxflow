#!/usr/bin/env bash
# Пакет Arch Linux (.pkg.tar.zst) из готового deb релиза.
#
# Запуск на машине с docker (CI - ubuntu-24.04):
#   script/build_arch_package.sh <путь к deb> <версия> <папка вывода>
# Результат: <папка вывода>/VoxFlow-<версия>-linux-x64.pkg.tar.zst.
#
# Два контейнера archlinux:latest:
#   1) сборка: makepkg от обычного пользователя (от root makepkg не работает),
#      проверка pacman -Qip и списка файлов;
#   2) проверка в чистом контейнере: pacman -U --noconfirm тянет depends из
#      репозиториев, затем ldd бинарников не должен найти «not found».
# Внутри контейнера скрипт зовёт сам себя с --in-container build|verify.
set -euo pipefail

ARCH_IMAGE="${VOXFLOW_ARCH_IMAGE:-archlinux:latest}"

die() {
  echo "build_arch_package: $*" >&2
  exit 1
}

# Совпадение без `cmd | grep -q`: под pipefail grep, вышедший на первом
# совпадении, роняет пайп через SIGPIPE.
# Песочница загрузчика pacman 7 (seccomp) не поднимается в части контейнеров
# и под эмуляцией amd64; контейнер одноразовый, поэтому выключаем её.
PACMAN_OPTS=(--noconfirm --disable-sandbox)

require_match() { # описание, текст, шаблон
  if ! grep -q -- "$3" <<<"$2"; then
    die "проверка не прошла: $1 (шаблон: $3)"
  fi
}

in_container_build() { # /in/voxflow.deb /in/LICENSE /scripts/arch/PKGBUILD -> /out
  local version="$1" deb_sha="$2" host_owner="$3"
  pacman -Syu "${PACMAN_OPTS[@]}" --needed fakeroot >/dev/null

  useradd -m builder
  local work=/home/builder/build
  install -d -o builder -g builder "$work"
  install -m644 -o builder -g builder /scripts/arch/PKGBUILD "$work/PKGBUILD"
  install -m644 -o builder -g builder /in/voxflow.deb "$work/voxflow.deb"
  install -m644 -o builder -g builder /in/LICENSE "$work/LICENSE"

  # --nodeps: depends ставит pacman у пользователя, здесь ничего не компилируем.
  su builder -c "cd '$work' && VOXFLOW_PKGVER='$version' VOXFLOW_DEB_SHA256='$deb_sha' \
    PKGEXT='.pkg.tar.zst' makepkg --nodeps --noconfirm --cleanbuild"

  local built=("$work"/voxflow-"$version"-1-x86_64.pkg.tar.zst)
  [[ ${#built[@]} -eq 1 && -s "${built[0]}" ]] || die "makepkg не собрал voxflow-$version-1-x86_64.pkg.tar.zst"
  local pkg="/out/VoxFlow-${version}-linux-x64.pkg.tar.zst"
  cp "${built[0]}" "$pkg"

  local info files
  info="$(pacman -Qip "$pkg")"
  files="$(pacman -Qlpq "$pkg")"
  echo "$info"
  require_match "имя пакета" "$info" '^Name *: voxflow$'
  require_match "версия пакета" "$info" "^Version *: ${version//./\\.}-1\$"
  require_match "зависимость webkit2gtk-4.1" "$info" 'webkit2gtk-4\.1'
  require_match "бинарник" "$files" '^/usr/bin/voxflow$'
  require_match ".desktop" "$files" '^/usr/share/applications/VoxFlow\.desktop$'
  require_match "иконка" "$files" '^/usr/share/icons/hicolor/.*/apps/voxflow\.png$'
  require_match "silero_vad.onnx" "$files" '/vad/silero_vad\.onnx$'
  chown "$host_owner" "$pkg"
}

in_container_verify() { # /out/<pkg>
  local version="$1"
  local pkg="/out/VoxFlow-${version}-linux-x64.pkg.tar.zst"
  [[ -s "$pkg" ]] || die "нет пакета $pkg"
  pacman -Syu "${PACMAN_OPTS[@]}" >/dev/null
  pacman -U "${PACMAN_OPTS[@]}" "$pkg" >/dev/null

  local installed
  installed="$(pacman -Qi voxflow)"
  require_match "установленная версия" "$installed" "^Version *: ${version//./\\.}-1\$"
  local owner
  owner="$(pacman -Qqo /usr/bin/voxflow)"
  [[ "$owner" == "voxflow" ]] || die "/usr/bin/voxflow принадлежит '$owner', а не voxflow"

  local bin=/usr/bin/voxflow out
  [[ -x "$bin" ]] || die "нет исполняемого $bin"
  out="$(ldd "$bin" 2>&1)" || die "ldd $bin: $out"
  if grep -q 'not found' <<<"$out"; then
    echo "$out" >&2
    die "$bin: не хватает библиотек - допишите пакеты в depends script/arch/PKGBUILD"
  fi
  echo "Пакет $pkg ставится в чистый archlinux, библиотеки на месте"
}

if [[ "${1:-}" == "--in-container" ]]; then
  mode="${2:?}"
  shift 2
  case "$mode" in
    build) in_container_build "$@" ;;
    verify) in_container_verify "$@" ;;
    *) die "неизвестный режим $mode" ;;
  esac
  exit 0
fi

[[ $# -eq 3 ]] || die "использование: $0 <deb> <версия> <папка вывода>"
deb="$1"
version="$2"
out="$3"
[[ -s "$deb" ]] || die "нет deb: $deb"
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "версия должна быть X.Y.Z: $version"
command -v docker >/dev/null || die "нужен docker"

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$script_dir/.." && pwd)"
mkdir -p "$out"
out_abs="$(cd "$out" && pwd)"
in_dir="$(mktemp -d)"
trap 'rm -rf "$in_dir"' EXIT
cp "$deb" "$in_dir/voxflow.deb"
cp "$repo_root/LICENSE" "$in_dir/LICENSE"
deb_sha="$(sha256sum "$in_dir/voxflow.deb" 2>/dev/null || shasum -a 256 "$in_dir/voxflow.deb")"
deb_sha="${deb_sha%% *}"

run_arch() {
  docker run --rm --platform linux/amd64 \
    -v "$in_dir:/in:ro" -v "$script_dir:/scripts:ro" -v "$out_abs:/out" \
    "$ARCH_IMAGE" bash /scripts/build_arch_package.sh --in-container "$@"
}

run_arch build "$version" "$deb_sha" "$(id -u):$(id -g)"
run_arch verify "$version"
echo "Готово: $out_abs/VoxFlow-${version}-linux-x64.pkg.tar.zst"
