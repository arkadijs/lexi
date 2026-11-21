#!/usr/bin/env bash
# Runs INSIDE the builder image (see devtools/android-builder). Usage: scripts/android-build.sh [gradle args...]
# Bootstraps the Gradle wrapper on first use (checksum from gradle.org/releases, 2026-11-21).
set -euo pipefail
cd "$(dirname "$0")/../android"
# The container runs as root; hand outputs back to the calling user so the next upload can overwrite them.
trap 'chown -R "${LEXI_UID:-0}:${LEXI_GID:-0}" /work/android 2>/dev/null || true' EXIT
GRADLE_VERSION=9.9.2
GRADLE_SHA256=971fcd7150ca4ba04852c0b2ca0d2d0ab9468e14b03473eab19528809914c76b

if [ ! -x gradlew ]; then
  curl -fsSL "https://services.gradle.org/distributions/gradle-$GRADLE_VERSION-bin.zip" -o /tmp/gradle.zip
  echo "$GRADLE_SHA256  /tmp/gradle.zip" | sha256sum -c -
  unzip -q /tmp/gradle.zip -d /tmp
  "/tmp/gradle-$GRADLE_VERSION/bin/gradle" wrapper --gradle-version "$GRADLE_VERSION" \
    --gradle-distribution-sha256-sum "$GRADLE_SHA256"
fi
[ "$#" -gt 0 ] || set -- assembleDebug
./gradlew --no-daemon --console=plain --max-workers "${LEXI_JOBS:-$(nproc)}" "$@"
