#!/usr/bin/env bash
# Installs the Android SDK packages Lexi needs via sdkmanager (SPEC §8) and prints env vars.
# Requires: JDK 17+, Android cmdline-tools on PATH (or ANDROID_HOME/cmdline-tools/latest/bin).
# Defaults verified 2026-11-21 against developer.android.com (Play target API 36, AGP 9.5 default build-tools 36.0.0, NDK r30 LTS).
# Override versions with PLATFORM / BUILD_TOOLS / NDK_VERSION.
set -euo pipefail

ANDROID_HOME="${ANDROID_HOME:-$HOME/Android/Sdk}"
PLATFORM="${PLATFORM:-android-36}"
BUILD_TOOLS="${BUILD_TOOLS:-36.0.0}"
NDK_VERSION="${NDK_VERSION:-30.0.16248370}"

SDKMANAGER="$(command -v sdkmanager || true)"
[ -n "$SDKMANAGER" ] || SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
[ -x "$SDKMANAGER" ] || { echo "sdkmanager not found; install Android cmdline-tools first" >&2; exit 1; }

mkdir -p "$ANDROID_HOME"
yes | "$SDKMANAGER" --sdk_root="$ANDROID_HOME" --licenses >/dev/null || true
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" \
  "platform-tools" "platforms;$PLATFORM" "build-tools;$BUILD_TOOLS" "ndk;$NDK_VERSION"

cat <<ENV

Add to your shell profile:
  export ANDROID_HOME="$ANDROID_HOME"
  export ANDROID_NDK_HOME="\$ANDROID_HOME/ndk/$NDK_VERSION"
  export PATH="\$PATH:\$ANDROID_HOME/platform-tools"
Then: rustup target add aarch64-linux-android && cargo install cargo-ndk
ENV
