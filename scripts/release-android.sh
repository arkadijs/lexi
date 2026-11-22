#!/usr/bin/env bash
# Build, zipalign and sign the release APK for sideloading -> dist/lexi-<version>.apk
#   scripts/release-android.sh [--install]
# Needs the key from scripts/release-keystore.sh (default ~/.config/lexi/release.{jks,pass}).
# Build-time settings (.envrc): LEXI_SERVICE_URL (required for a release build) and the attestation key:
#   LEXI_ATTEST_KEY=<key>   a specific key, e.g. one friend's (see docs/RUNBOOK.md); otherwise the first key
#                           of the cluster Secret is used.
# The keystore and its password travel over ssh stdin only and are removed from the build host afterwards.
set -euo pipefail
DIR="${LEXI_RELEASE_DIR:-$HOME/.config/lexi}"
cd "$(dirname "$0")/.."
. scripts/env.sh
lexi_need_host
[ -f "$DIR/release.jks" ] && [ -f "$DIR/release.pass" ] || { echo "no signing key: run scripts/release-keystore.sh first" >&2; exit 1; }

export LEXI_SERVICE_URL
if [ -z "${LEXI_ATTEST_KEY:-}" ]; then export LEXI_ATTEST_FROM_CLUSTER=1; fi
rm -f dist/app-release-unsigned.apk
scripts/android-remote.sh assembleRelease
[ -f dist/app-release-unsigned.apk ] || { echo "no unsigned release APK was produced" >&2; exit 1; }

VERSION="$(sed -n 's/.*versionName = "\(.*\)".*/\1/p' android/app/build.gradle.kts | head -1)"
OUT="dist/lexi-$VERSION.apk"
echo ">> aligning and signing"
ssh "$HOST" "rm -rf /tmp/lexi-sign && mkdir -p /tmp/lexi-sign && cat > /tmp/lexi-sign/in.apk" < dist/app-release-unsigned.apk
KS_B64="$(base64 -w0 "$DIR/release.jks")"; PASS="$(cat "$DIR/release.pass")"
ssh "$HOST" bash -s <<REMOTE
set -e
KS=\$(mktemp -p /dev/shm)
trap 'rm -f \$KS' EXIT
echo '$KS_B64' | base64 -d > \$KS
chmod 644 \$KS
PASS='$PASS' docker run --rm -e PASS -v \$KS:/ks.jks:ro -v /tmp/lexi-sign:/s lexi-android-builder bash -c '
  BT=\$(ls -d \$ANDROID_HOME/build-tools/* | sort -V | tail -1)
  \$BT/zipalign -f -p 4 /s/in.apk /s/aligned.apk
  \$BT/apksigner sign --ks /ks.jks --ks-key-alias lexi --ks-pass env:PASS --out /s/out.apk /s/aligned.apk
  \$BT/apksigner verify --print-certs /s/out.apk | grep -E "Signer #1 certificate (DN|SHA-256)|Verifies"
  chmod 644 /s/out.apk'
REMOTE
scp -q "$HOST:/tmp/lexi-sign/out.apk" "$OUT"
ssh "$HOST" "rm -rf /tmp/lexi-sign"
echo ">> $OUT ($(stat -c %s "$OUT") bytes)"
if [ "${1:-}" = "--install" ]; then
  adb install -r "$OUT" || { echo "signature differs from the installed app: adb uninstall app.lexi, then retry" >&2; exit 1; }
fi
