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
# Guard: a cdylib links fine with undefined symbols and only fails when Android loads it (an app once shipped
# that crashed at startup: "cannot locate symbol eglSwapInterval"). Every undefined symbol of a family we use
# must have its library in NEEDED. Uses the NDK's llvm tools (the container's binutils may not read aarch64).
check_so() {
  local so="$1" bin="${ANDROID_NDK_HOME}/toolchains/llvm/prebuilt/linux-x86_64/bin" bad=0 undef needed prefix lib pair
  undef=$("$bin/llvm-nm" -D --undefined-only "$so" | awk '{print $NF}')
  needed=$("$bin/llvm-readelf" -d "$so" | grep NEEDED || true)
  for pair in "egl:libEGL.so" "AAudio:libaaudio.so" "ANativeWindow:libandroid.so" "AChoreographer:libandroid.so" \
              "ALooper:libandroid.so" "__android_log:liblog.so"; do
    prefix=${pair%%:*}; lib=${pair##*:}
    if echo "$undef" | grep -q "^$prefix" && ! echo "$needed" | grep -q "$lib"; then
      echo "ERROR: $so uses $prefix* symbols but is not linked against $lib (it would crash at load time)" >&2
      bad=1
    fi
  done
  [ "$bad" = 0 ] && echo "check_so: $(basename "$so") links every library it needs"
  return $bad
}

rc=0
./gradlew --no-daemon --console=plain --max-workers "${LEXI_JOBS:-$(nproc)}" "$@" || rc=$?
so=app/src/main/jniLibs/arm64-v8a/liblexi_core.so
if [ "$rc" = 0 ] && [ -f "$so" ]; then check_so "$so" || rc=1; fi
exit "$rc"
