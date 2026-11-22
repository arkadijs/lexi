#!/usr/bin/env bash
# One-time: create the release signing key for sideloaded builds, OUTSIDE the repo (default ~/.config/lexi).
#   scripts/release-keystore.sh
# BACK UP release.jks and release.pass (password manager or an encrypted drive). A lost key means every
# friend must uninstall Lexi before they can install an update (Android refuses a different signature).
# The key is generated in the builder container on the build host (no JDK needed locally); the password goes over stdin.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/env.sh
lexi_need_host
DIR="${LEXI_RELEASE_DIR:-$HOME/.config/lexi}"
mkdir -p "$DIR"; chmod 700 "$DIR"
if [ -e "$DIR/release.jks" ]; then echo "refusing to overwrite $DIR/release.jks" >&2; exit 1; fi
umask 077
PASS="$(openssl rand -hex 24)"
printf %s "$PASS" | ssh "$HOST" 'docker run --rm -i lexi-android-builder bash -c '"'"'
  read -r P
  keytool -genkeypair -keystore /tmp/k.jks -storetype PKCS12 -alias lexi -keyalg RSA -keysize 3072 \
    -validity 10000 -dname "CN=Lexi, O=Lexi friends" -storepass "$P" -keypass "$P" >/dev/null 2>&1
  base64 -w0 /tmp/k.jks'"'" | base64 -d > "$DIR/release.jks"
printf %s "$PASS" > "$DIR/release.pass"
chmod 600 "$DIR/release.jks" "$DIR/release.pass"
echo "created $DIR/release.jks and $DIR/release.pass (alias lexi, RSA 3072, valid ~27 years)"
echo "BACK THEM UP NOW. Then: scripts/release-android.sh"
