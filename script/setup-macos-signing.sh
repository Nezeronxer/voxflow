#!/usr/bin/env bash
set -euo pipefail

# One-time setup of the permanent VoxFlow self-signed code-signing key.
#
# Run once on the owner's Mac:  ./script/setup-macos-signing.sh
#
# Creates a 10-year self-signed certificate (CN "VoxFlow Self-Signed", EKU
# codeSigning), stores it as the GitHub secrets MACOS_SELF_SIGNED_P12_BASE64 and
# MACOS_SELF_SIGNED_P12_PASSWORD, then imports it into the login Keychain with
# code-signing trust. Release builds signed with this key keep the same
# designated requirement, so macOS asks for microphone/accessibility once.
# No secret value is ever printed; temporary files live in a 700 directory and
# are removed on exit. If any step fails after the import, the import is rolled
# back, so a rerun starts clean.
#
# Options:
#   --repo OWNER/NAME   GitHub repository (default: Nezeronxer/voxflow)
#   --keychain PATH     Local-only mode for checks: import into PATH, skip the
#                       trust prompt and the GitHub upload.

readonly IDENTITY_NAME="VoxFlow Self-Signed"
readonly SECRET_P12="MACOS_SELF_SIGNED_P12_BASE64"
readonly SECRET_PASSWORD="MACOS_SELF_SIGNED_P12_PASSWORD"
# LibreSSL writes a PKCS#12 that `security import` reads; OpenSSL 3 needs -legacy.
readonly OPENSSL="/usr/bin/openssl"

repo="Nezeronxer/voxflow"
keychain="$HOME/Library/Keychains/login.keychain-db"
local_only=0

die() {
  echo "Ошибка: $*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo) [[ $# -ge 2 ]] || die "--repo требует значение"; repo="$2"; shift 2 ;;
    --keychain) [[ $# -ge 2 ]] || die "--keychain требует путь"; keychain="$2"; local_only=1; shift 2 ;;
    -h|--help) sed -n '3,21p' "$0"; exit 0 ;;
    *) die "неизвестный аргумент: $1" ;;
  esac
done

[[ "$(uname -s)" == "Darwin" ]] || die "скрипт работает только на macOS"

identities="$(/usr/bin/security find-identity -p codesigning "$keychain" 2>/dev/null || true)"
if grep -qF "\"$IDENTITY_NAME\"" <<< "$identities"; then
  if [[ "$local_only" == 1 ]]; then
    echo "Ключ \"$IDENTITY_NAME\" уже есть в $keychain. Второй не создаю."
    exit 0
  fi
  command -v gh >/dev/null || die "не найден gh (GitHub CLI)"
  secrets="$(gh secret list --repo "$repo" 2>/dev/null)" \
    || die "не удалось прочитать секреты $repo: проверьте gh auth login и доступ к репозиторию"
  if grep -qw "$SECRET_P12" <<< "$secrets" && grep -qw "$SECRET_PASSWORD" <<< "$secrets"; then
    echo "Ключ \"$IDENTITY_NAME\" уже есть, секреты в $repo на месте. Ничего не делаю."
    exit 0
  fi
  # Пароль p12 нигде не хранится, поэтому секреты для старого ключа выставить нельзя.
  die "ключ \"$IDENTITY_NAME\" есть в связке ключей, но секретов в $repo нет. Удалите сертификат \"$IDENTITY_NAME\" в Связке ключей (вместе с закрытым ключом) и запустите скрипт ещё раз"
fi

if [[ "$local_only" == 0 ]]; then
  command -v gh >/dev/null || die "не найден gh (GitHub CLI)"
  gh auth status >/dev/null 2>&1 || die "gh не авторизован: выполните gh auth login"
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/voxflow-signing.XXXXXX")" || die "не удалось создать временную папку"
chmod 700 "$work"
imported=0
cleanup() {
  local status=$?
  # Любой сбой после импорта: убираем ключ, чтобы повторный запуск не упёрся в тупик.
  if [[ "$status" != 0 && "$imported" == 1 ]]; then
    /usr/bin/security delete-identity -c "$IDENTITY_NAME" "$keychain" >/dev/null 2>&1 || true
    echo "Импорт откатан: ключа в связке ключей нет, можно запускать заново." >&2
  fi
  rm -rf "$work"
}
trap cleanup EXIT
umask 077

cat > "$work/openssl.cnf" <<EOF
[req]
distinguished_name = dn
x509_extensions = ext
prompt = no
[dn]
CN = $IDENTITY_NAME
[ext]
basicConstraints = critical,CA:false
keyUsage = critical,digitalSignature
extendedKeyUsage = critical,codeSigning
EOF

"$OPENSSL" req -x509 -newkey rsa:2048 -nodes -sha256 -days 3650 \
  -keyout "$work/key.pem" -out "$work/cert.pem" -config "$work/openssl.cnf" 2>/dev/null \
  || die "openssl не смог создать сертификат ($OPENSSL)"
"$OPENSSL" rand -hex 24 | tr -d '\n' > "$work/password" \
  || die "openssl не смог сгенерировать пароль"
"$OPENSSL" pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
  -name "$IDENTITY_NAME" -passout "file:$work/password" -out "$work/identity.p12" 2>/dev/null \
  || die "openssl не смог собрать p12"

# Сначала секреты: если выгрузка упадёт, в связке ключей ещё ничего нет и повтор чистый.
if [[ "$local_only" == 0 ]]; then
  base64 < "$work/identity.p12" | tr -d '\n' | gh secret set "$SECRET_P12" --repo "$repo" >/dev/null \
    || die "gh не смог записать $SECRET_P12 в $repo"
  gh secret set "$SECRET_PASSWORD" --repo "$repo" < "$work/password" >/dev/null \
    || die "gh не смог записать $SECRET_PASSWORD в $repo (повторный запуск перезапишет оба секрета)"
fi

# `security import` принимает пароль только аргументом (на миг виден процессам этого
# же пользователя); в терминал он не выводится.
/usr/bin/security import "$work/identity.p12" -k "$keychain" \
  -P "$(cat "$work/password")" -T /usr/bin/codesign >/dev/null \
  || die "не удалось импортировать ключ в $keychain"
imported=1

if [[ "$local_only" == 0 ]]; then
  # macOS один раз спросит пароль входа, чтобы доверить сертификат для подписи кода.
  /usr/bin/security add-trusted-cert -p codeSign -k "$keychain" "$work/cert.pem" \
    || die "не удалось доверить сертификат для подписи кода"
  echo "Готово: ключ \"$IDENTITY_NAME\" в связке ключей, секреты $SECRET_P12 и $SECRET_PASSWORD в $repo."
else
  echo "Готово (локально): ключ \"$IDENTITY_NAME\" импортирован в $keychain, GitHub не трогался."
fi
