#!/usr/bin/env bash
# Build the Android app on remote (x86_64; Google's build tools don't run on this aarch64 dev box).
#   scripts/android-remote.sh [gradle args]      # default: assembleDebug; APK is copied to dist/
# Ships the working tree (never .envrc/.git/target), runs the builder image, copies results back.
set -euo pipefail
HOST="${LEXI_DEPLOY_HOST:-deploy.example.com}"
SRC=/tmp/lexi-android-src
cd "$(dirname "$0")/.."

if ! ssh "$HOST" 'docker image inspect lexi-android-builder >/dev/null 2>&1'; then
  echo ">> building lexi-android-builder (first time, several minutes)"
  tar -cf - devtools/android-builder scripts/setup-android-sdk.sh \
    | ssh "$HOST" 'rm -rf /tmp/lexi-abuild && mkdir -p /tmp/lexi-abuild && tar -C /tmp/lexi-abuild -xf - \
        && cd /tmp/lexi-abuild && docker build -t lexi-android-builder -f devtools/android-builder/Dockerfile .'
fi

echo ">> shipping working tree"
tar --exclude=.git --exclude=target --exclude=.envrc --exclude='.env*' --exclude=dist \
    --exclude=android/.gradle --exclude=android/build --exclude=android/app/build \
    --exclude=android/app/src/main/jniLibs -cf - . \
  | ssh "$HOST" "mkdir -p $SRC && tar -C $SRC -xf -"

# Use every core of the build host (remote has 80): no --cpus limit, and all parallelism knobs set explicitly.
JOBS="$(ssh "$HOST" nproc)"
echo ">> building with $JOBS jobs (gradle ${*:-assembleDebug})"
ssh "$HOST" "docker run --rm -e LEXI_UID=\$(id -u) -e LEXI_GID=\$(id -g) -e LEXI_JOBS=$JOBS -e CARGO_BUILD_JOBS=$JOBS -e CMAKE_BUILD_PARALLEL_LEVEL=$JOBS \
  -e MAKEFLAGS=-j$JOBS -v $SRC:/work -v lexi-gradle:/root/.gradle -v lexi-target:/work/target \
  -w /work lexi-android-builder scripts/android-build.sh $*"

echo ">> fetching results"
mkdir -p dist
ssh "$HOST" "cd $SRC/android/app/build/outputs && find . -name '*.apk' -o -name '*.aab'" | while read -r f; do
  scp -q "$HOST:$SRC/android/app/build/outputs/$f" "dist/$(basename "$f")" && echo "dist/$(basename "$f")"
done
# keep the generated Gradle wrapper in the repo
if [ ! -f android/gradlew ]; then
  scp -q "$HOST:$SRC/android/gradlew" android/gradlew
  mkdir -p android/gradle/wrapper
  scp -q "$HOST:$SRC/android/gradle/wrapper/gradle-wrapper.jar" "$HOST:$SRC/android/gradle/wrapper/gradle-wrapper.properties" android/gradle/wrapper/
fi
