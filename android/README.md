# Lexi Android shell

Thin Kotlin shell (no Compose, **no dependencies at all**: minSdk 31 platform APIs cover everything) around
`liblexi_core.so`. The JNI surface is `NativeBridge.kt` ⇄ `core/src/ffi/mod.rs` (SPEC §4.2).

## Build

Google's build tools are x86_64-only, so on this aarch64 dev box the app is built in a container on remote:

```sh
scripts/android-remote.sh                 # assembleDebug  -> dist/app-debug.apk
scripts/android-remote.sh assembleRelease # R8 on          -> dist/app-release-unsigned.apk
```

The first run builds `lexi-android-builder` (JDK 17, SDK, NDK r30, Rust + cargo-ndk; `devtools/android-builder`).
The build uses all cores of the host. On an x86_64 machine with the SDK, `scripts/setup-android-sdk.sh` then
`cd android && ./gradlew assembleDebug` is equivalent: the `buildRust` Gradle task runs
`cargo ndk -t arm64-v8a --platform 31 … build -p lexi-core --release` before `preBuild`
(`-PwithArmv7` adds armeabi-v7a; needs `rustup target add armv7-linux-androideabi`, untested).

The minter URL is a `BuildConfig` field: debug `http://10.0.2.2:8080` (cleartext allowed for that host in the
debug source set only), release `https://lexi-minter.example.com`.

## Verified (2026-11-21) against developer.android.com

- Google Play: new apps and updates must target **API 36** (from 2026-08-31) → `compileSdk = targetSdk = 36`.
- AGP **9.5.1** (needs Gradle ≥ 9.6, JDK 17, build-tools 36.0.0) with Gradle **9.9.2**; Kotlin is built into AGP 9
  (the build works without the `kotlin-android` plugin).
- NDK r30 LTS (`30.0.16248370`); command-line tools `commandlinetools-linux-15859902` (checksum pinned).
- Microphone foreground service: manifest type `microphone` + `FOREGROUND_SERVICE_MICROPHONE`, must be started from a
  visible Activity (we do), `RECORD_AUDIO` granted first.

## What was checked on the build host (no device available)

- Debug and release APKs build; release is **600 KB** (`.so` 532 KB), arm64-v8a only.
- All 12 `Java_app_lexi_NativeBridge_*` exports are in the packaged `.so`; R8 keeps the native methods and
  `onState` (checked in the release dex). Merged manifest: `foregroundServiceType=microphone`, `targetSdk 36`,
  release has no cleartext config.
- `cargo clippy -D warnings` is clean for `aarch64-linux-android` (this is the only place `core/ffi` is compiled).

## Not verified (needs a device or emulator)

Buttons rendering, the permission flow, the foreground service/notification actions, audio routing, wake/Wi-Fi
locks, and network-change handling were written to SPEC §6 but never run. Native calls are stubs that log to
logcat (`adb logcat -s lexi`) and fake Connecting → Live; real audio/RTC wiring is Phase 5, rendering Phase 6.

## Known gap (SPEC)

The deployed minter runs `shared-key` attestation, but `startCall(minterUrl, installIdHash)` (SPEC §4.2) has no
way to carry an `X-Lexi-Attest` value. Phase 5 needs either an extra parameter or a Kotlin-supplied header.
