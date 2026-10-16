#!/usr/bin/env bash
# Установка VoxFlow на Mac одной командой:
#   curl -fsSL https://raw.githubusercontent.com/Nezeronxer/voxflow/master/script/install-macos.sh | bash
# Переменные: VOXFLOW_VERSION=x.y.z - конкретный релиз; VOXFLOW_DRY_RUN=1 - всё до замены, без замены.
# Вся логика в main, вызов - последней строкой: при обрыве curl | bash не выполнится половина скрипта.
set -euo pipefail

REPO="Nezeronxer/voxflow"
APPS="/Applications"
APP="$APPS/VoxFlow.app"
MIN_MACOS=11
DRY_RUN="${VOXFLOW_DRY_RUN:-0}"

TMP=""
MNT=""
STAGED=""
BACKUP=""

die() { echo "Ошибка: $*" >&2; exit 1; }
say() { echo "VoxFlow: $*"; }

cleanup() {
  # Прерывание между двумя mv: старая версия лежит в BACKUP, а на месте приложения пусто.
  if [ -n "$BACKUP" ] && [ ! -d "$APP" ] && [ -d "$BACKUP" ]; then mv "$BACKUP" "$APP" || true; fi
  if [ -n "$STAGED" ]; then rm -rf "$STAGED"; fi
  if [ -n "$MNT" ]; then
    hdiutil detach "$MNT" -quiet >/dev/null 2>&1 || hdiutil detach -force "$MNT" -quiet >/dev/null 2>&1 || true
    # Если образ так и не отмонтировался, rm -rf пошёл бы по его содержимому - оставляем временную папку.
    if mount | grep -qF -- " on $MNT "; then TMP=""; fi
  fi
  if [ -n "$TMP" ]; then rm -rf "$TMP"; fi
}

app_version() { defaults read "$1/Contents/Info" CFBundleShortVersionString 2>/dev/null || true; }

main() {
  trap cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM

  [ "$(uname -s)" = "Darwin" ] || die "этот установщик только для macOS."
  # uname -m под Rosetta врёт, поэтому спрашиваем ядро.
  [ "$(sysctl -n hw.optional.arm64 2>/dev/null || echo 0)" = "1" ] \
    || die "VoxFlow пока работает только на Mac с чипом Apple (M1 и новее). Intel-маки не поддерживаются."
  [ "$(sw_vers -productVersion | cut -d. -f1)" -ge "$MIN_MACOS" ] \
    || die "нужна macOS $MIN_MACOS или новее. Обновите систему в Системных настройках."
  [ -w "$APPS" ] \
    || die "нет прав на запись в $APPS. Войдите под администратором или перетащите VoxFlow.app из .dmg вручную."

  local api release urls dmg_urls sums_url name want have new_ver cur_ver
  if [ -n "${VOXFLOW_VERSION:-}" ]; then
    api="https://api.github.com/repos/$REPO/releases/tags/v${VOXFLOW_VERSION#v}"
  else
    api="https://api.github.com/repos/$REPO/releases/latest"
  fi
  say "ищу релиз..."
  release="$(curl -fsSL -H "Accept: application/vnd.github+json" "$api")" \
    || die "не удалось получить данные о релизе. Проверьте интернет и попробуйте ещё раз."
  # || true: релиз без ассетов даёт пустой результат grep, дальше его ловят die с понятной фразой.
  urls="$(printf '%s\n' "$release" | grep -o '"browser_download_url": *"[^"]*"' | sed 's/.*: *"//; s/"$//' || true)"

  dmg_urls="$(printf '%s\n' "$urls" | grep -E -- '-arm64[^/]*\.dmg$' || true)"
  [ -n "$dmg_urls" ] || die "в релизе нет образа .dmg для Mac с чипом Apple."
  [ "$(printf '%s\n' "$dmg_urls" | wc -l)" -eq 1 ] || die "в релизе несколько образов .dmg, не понятно, какой ставить."
  # В релизах 2.x файл называется SHA256SUMS.txt, в новых допускаем и SHA256SUMS.
  sums_url="$(printf '%s\n' "$urls" | grep -E '/SHA256SUMS(\.txt)?$' | head -n 1 || true)"
  [ -n "$sums_url" ] || die "в релизе нет файла с контрольными суммами. Установка остановлена."

  # pwd -P: mount показывает /private/var/..., а mktemp даёт /var/... - без этого проверка в cleanup не совпала бы никогда.
  TMP="$(cd "$(mktemp -d)" && pwd -P)"
  name="${dmg_urls##*/}"
  say "скачиваю $name..."
  curl -fSL --progress-bar -o "$TMP/$name" "$dmg_urls" || die "не удалось скачать образ. Проверьте интернет и попробуйте ещё раз."
  curl -fsSL -o "$TMP/sums" "$sums_url" || die "не удалось скачать контрольные суммы."

  want="$(awk -v n="$name" '{f=$2; sub(/^\*/, "", f); if (f == n) print $1}' "$TMP/sums" | head -n 1)"
  [ -n "$want" ] || die "в файле контрольных сумм нет записи для $name."
  have="$(shasum -a 256 "$TMP/$name" | awk '{print $1}')"
  [ "$want" = "$have" ] || die "контрольная сумма не совпала, файл повреждён. Установленная версия не тронута."
  say "контрольная сумма сошлась."

  mkdir "$TMP/mnt"
  MNT="$TMP/mnt"
  hdiutil attach -nobrowse -readonly -quiet -mountpoint "$MNT" "$TMP/$name" >/dev/null \
    || die "не удалось открыть образ."
  [ -d "$MNT/VoxFlow.app" ] || die "в образе нет VoxFlow.app."

  new_ver="$(app_version "$MNT/VoxFlow.app")"
  cur_ver=""
  if [ -d "$APP" ]; then cur_ver="$(app_version "$APP")"; fi

  if [ -n "$cur_ver" ] && [ "$cur_ver" = "$new_ver" ]; then
    say "уже стоит последняя версия ($cur_ver)."
    if [ "$DRY_RUN" != "1" ]; then open "$APP"; fi
    return 0
  fi

  if [ "$DRY_RUN" = "1" ]; then
    say "пробный запуск: версия $new_ver скачана, сумма сошлась, образ открывается (сейчас стоит: ${cur_ver:-ничего}). Замены не было."
    return 0
  fi

  # Копия рядом с /Applications, чтобы финальный mv был атомарным (тот же том).
  STAGED="$APPS/.VoxFlow-install-$$.app"
  rm -rf "$STAGED"
  ditto "$MNT/VoxFlow.app" "$STAGED" || die "не удалось скопировать программу. Установленная версия не тронута."
  xattr -dr com.apple.quarantine "$STAGED" 2>/dev/null || true
  codesign --verify --deep --strict "$STAGED" 2>/dev/null \
    || die "подпись программы не прошла проверку. Установленная версия не тронута."

  if pgrep -xi voxflow >/dev/null; then
    say "закрываю запущенный VoxFlow..."
    osascript -e 'tell application "VoxFlow" to quit' >/dev/null 2>&1 || true
    for _ in 1 2 3 4 5 6 7 8 9 10; do pgrep -xi voxflow >/dev/null || break; sleep 1; done
    pkill -xi voxflow 2>/dev/null || true
    for _ in 1 2 3 4 5; do pgrep -xi voxflow >/dev/null || break; sleep 1; done
    if pgrep -xi voxflow >/dev/null; then die "VoxFlow не закрывается. Закройте его вручную и запустите команду снова."; fi
  fi

  if [ -d "$APP" ]; then
    BACKUP="$APPS/.VoxFlow-old-$$.app"
    mv "$APP" "$BACKUP" || { BACKUP=""; die "не удалось убрать старую версию."; }
  fi
  if ! mv "$STAGED" "$APP"; then
    die "не удалось поставить новую версию, старая возвращена."
  fi
  STAGED=""
  if [ -n "$BACKUP" ]; then rm -rf "$BACKUP"; BACKUP=""; fi

  say "версия $new_ver установлена в $APPS."
  open "$APP"
}

main "$@"
