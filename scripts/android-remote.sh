#!/usr/bin/env bash
# Build the Android app on the build host $LEXI_DEPLOY_HOST (x86_64 Linux with docker; Google's build tools do not run
# on every dev machine, e.g. aarch64). The toolchain lives in the container image devtools/android-builder.
#   scripts/android-remote.sh [gradle args]      # default: assembleDebug; APK is copied to dist/
# Build-time settings (see android/app/build.gradle.kts):
#   LEXI_SERVICE_URL=https://lexi.example.com  # from .envrc; debug default http://localhost:8080 (adb reverse), release requires it
#   LEXI_ATTEST_FROM_CLUSTER=1                        # read the shared attestation key from the cluster Secret
#   LEXI_ATTEST_KEY=<key>                             # or give one key explicitly (per-friend builds)
#   LEXI_COLD=1                                       # throwaway target volume = full dependency compile (use with --rerun-tasks)
# Every build prints a [measure] summary of how many cores it really kept busy (scripts/measure-lib.sh).
# Ships the working tree (never .envrc/.git/target), runs the builder image, copies results back.
set -euo pipefail
SRC=/tmp/lexi-android-src
cd "$(dirname "$0")/.."
. scripts/env.sh
lexi_need_host
lexi_warn_no_keys

if ! ssh "$HOST" 'docker image inspect lexi-android-builder >/dev/null 2>&1'; then
  echo ">> building lexi-android-builder (first time, several minutes)"
  tar -cf - devtools/android-builder scripts/setup-android-sdk.sh \
    | ssh "$HOST" 'rm -rf /tmp/lexi-abuild && mkdir -p /tmp/lexi-abuild && tar -C /tmp/lexi-abuild -xf - \
        && cd /tmp/lexi-abuild && docker build -t lexi-android-builder -f devtools/android-builder/Dockerfile .'
fi

# Self-heal: a container run by hand (as root) leaves root-owned files that tar could not overwrite.
ssh "$HOST" "[ -d $SRC ] && docker run --rm -v $SRC:/work alpine chown -R \$(id -u):\$(id -g) /work || true" >/dev/null 2>&1
echo ">> shipping working tree"
tar --exclude=.git --exclude=target --exclude=.envrc --exclude='.env*' --exclude=dist \
    --exclude=android/.gradle --exclude=android/build --exclude=android/app/build \
    --exclude=android/app/src/main/jniLibs -cf - . \
  | ssh "$HOST" "mkdir -p $SRC && tar -C $SRC -xf -"

# Use every core of the build host: no --cpus limit, and all parallelism knobs set explicitly.
JOBS="$(ssh "$HOST" nproc)"
ENVFILE=""
if [ "${LEXI_ATTEST_FROM_CLUSTER:-}" = 1 ]; then
  # Via process substitution + printf builtin: the key never appears in any command line.
  ENVFILE="--env-file <(printf 'LEXI_ATTEST_KEY=%s\\n' \"\$(kubectl -n lexi get secret lexi-attest-key -o jsonpath='{.data.LEXI_ATTEST_SHARED_KEY}' | base64 -d | cut -d, -f1)\")"
fi
if [ -n "${LEXI_ATTEST_KEY:-}" ]; then
  # A specific key (e.g. one friend's): it travels in the stdin script (never in a command line).
  ENVFILE="--env-file <(printf 'LEXI_ATTEST_KEY=%s\\n' '$LEXI_ATTEST_KEY')"
fi
SERVICE_ENV=""
[ -n "${LEXI_SERVICE_URL:-}" ] && SERVICE_ENV="-e LEXI_SERVICE_URL=$LEXI_SERVICE_URL"
echo ">> building with $JOBS jobs (gradle ${*:-assembleDebug})"
# The remote command is a script on stdin (measure-lib.sh + one measured docker run), so the env-file
# process substitution needs no extra quoting. LEXI_COLD=1 uses a throwaway target volume: a full
# compile of every dependency, which is what shows how parallel the build can really be.
TARGET_VOL=lexi-target
if [ "${LEXI_COLD:-}" = 1 ]; then TARGET_VOL=lexi-target-cold; ssh "$HOST" "docker volume rm -f $TARGET_VOL >/dev/null"; fi
{
  cat scripts/measure-lib.sh
  cat <<REMOTE
measure_run "android build (gradle ${*:-assembleDebug})" docker run --rm $SERVICE_ENV $ENVFILE -e LEXI_UID=\$(id -u) -e LEXI_GID=\$(id -g) -e LEXI_JOBS=$JOBS \\
  -e CARGO_BUILD_JOBS=$JOBS -e CMAKE_BUILD_PARALLEL_LEVEL=$JOBS -e MAKEFLAGS=-j$JOBS \\
  -v $SRC:/work -v lexi-gradle:/root/.gradle -v lexi-android:/root/.android -v $TARGET_VOL:/work/target \\
  -w /work lexi-android-builder scripts/android-build.sh $*
rc=\$?
[ "$TARGET_VOL" = lexi-target-cold ] && docker volume rm -f $TARGET_VOL >/dev/null
exit \$rc
REMOTE
} | ssh "$HOST" bash -s

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
