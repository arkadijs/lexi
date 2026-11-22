# Lexi Android shell

Thin Kotlin shell (no Compose, **no dependencies at all**: minSdk 31 platform APIs cover everything) around
`liblexi_core.so`. The JNI surface is `NativeBridge.kt` ⇄ `core/src/ffi/mod.rs` (SPEC §4.2).

## Build

Google's build tools are x86_64-only, so the app is built in a container on a build host (`LEXI_DEPLOY_HOST`, an
x86_64 Linux machine with docker; the toolchain image is built there on the first run). Settings come from `.envrc`:

```sh
scripts/android-remote.sh                 # assembleDebug  -> dist/app-debug.apk
scripts/android-remote.sh assembleRelease # R8 on          -> dist/app-release-unsigned.apk
```

The first run builds `lexi-android-builder` (JDK 17, SDK, NDK r30, Rust + cargo-ndk; `devtools/android-builder`).
The build uses all cores of the host. On an x86_64 machine with the SDK, `scripts/setup-android-sdk.sh` then
`cd android && ./gradlew assembleDebug` is equivalent: the `buildRust` Gradle task runs
`cargo ndk -t arm64-v8a --platform 31 … build -p lexi-core --release` before `preBuild`
(`-PwithArmv7` adds armeabi-v7a; needs `rustup target add armv7-linux-androideabi`, untested).

### Build-time settings (never committed)

| Setting (env / `-P`) | Default | Meaning |
|---|---|---|
| `LEXI_SERVICE_URL` / `lexiServiceUrl` | debug `http://localhost:8080`; **required** for a release build (the build fails without it) | `BuildConfig.SERVICE_URL`. Cleartext is allowed to `localhost` in debug only (use `adb reverse tcp:8080 tcp:8080` to reach a service on the dev machine). |
| `LEXI_ATTEST_KEY` / `lexiAttestKey` | empty (no attestation) | Shared key for the service's `shared-key` mode. It ships inside the APK (weak, but enough for friends: per-friend keys are revocable, see `docs/RUNBOOK.md`). |

`scripts/android-remote.sh` forwards `LEXI_SERVICE_URL`, and with `LEXI_ATTEST_FROM_CLUSTER=1` reads the key from the
cluster Secret (via an env-file, so it never appears on a command line). Example for a phone that talks to the
deployed service (`LEXI_SERVICE_URL` comes from `.envrc`):

```sh
LEXI_ATTEST_FROM_CLUSTER=1 scripts/android-remote.sh assembleDebug
adb install -r dist/app-debug.apk
```

Debug builds are signed with a key kept in the `lexi-android` docker volume on the build host, so updates install
over each other. (Without that volume every build got a new key and `adb install -r` failed with
`INSTALL_FAILED_UPDATE_INCOMPATIBLE`.)

## Verified (2026-11-21) against developer.android.com

- Google Play requires **API 36** for new apps and updates (from 2026-08-31). The app is not published on Play
  (sideloaded for the author and friends), but `targetSdk = 36` is kept to avoid compatibility behaviours.
- AGP **9.5.1** (needs Gradle ≥ 9.6, JDK 17, build-tools 36.0.0) with Gradle **9.9.2**; Kotlin is built into AGP 9
  (the build works without the `kotlin-android` plugin).
- NDK r30 LTS (`30.0.16248370`); command-line tools `commandlinetools-linux-15859902` (checksum pinned).
- Microphone foreground service: manifest type `microphone` + `FOREGROUND_SERVICE_MICROPHONE`, must be started from a
  visible Activity (we do), `RECORD_AUDIO` granted first.

## What was checked on the build host

- Debug and release APKs build; release is **600 KB** (`.so` 532 KB), arm64-v8a only.
- All 12 `Java_app_lexi_NativeBridge_*` exports are in the packaged `.so`; R8 keeps the native methods and
  `onState` (checked in the release dex). Merged manifest: `foregroundServiceType=microphone`, `targetSdk 36`,
  release has no cleartext config.
- `cargo clippy -D warnings` is clean for `aarch64-linux-android` (this is the only place `core/ffi` is compiled).

## Verified on a real device (Moto g42, Android 13, no SIM, Wi-Fi only; 2026-11-21)

All with a **muted** microphone, so no room audio was sent (the mic ring is drained and the encode is skipped).

| Check | Result |
|---|---|
| Mute before the call | Works: button shows "Muted", `setMuted` precedes `startCall`, call starts silent. |
| Tap Call → live (token + SDP + ICE/DTLS, via the deployed service with attestation) | 1.2 s, 1.5 s, 2.0 s, 2.0 s, 2.35 s (the very first, cold call: 3.0 s). Of that: token 0.4–1.3 s, SDP exchange 0.6–1.1 s, ICE/DTLS 0.3–0.5 s. Target < 2.5 s. This is "media connected", not first audio. |
| Route switch mid-call (speaker ↔ earpiece) | The AAudio streams are re-opened, the call stays live. Bluetooth/wired not tested. |
| Wi-Fi off for 4 s, then on | `onNetworkChanged` → reconnect with a fresh token → live again 1.3 s after Wi-Fi returned. The Wi-Fi → cellular handover is **untested** (no SIM). |
| Call survives the screen off | A muted call ran ~12 min, ~10 of them with the screen off, with no reconnects or drops. |
| Hang up from the in-app button and from the notification action | Both work: service stops, wake lock released, AAudio streams closed, audio mode reset. |
| CPU (muted call, % of one core, measured from `/proc` ticks) | Screen on 6.4 %; screen off 10.1 % (it was 23 % before skipping the encode while muted, Opus complexity 5, and 20 ms AAudio callbacks). **Still above the 5 % budget (SPEC N4).** Screen off: `lexi-net` 5.0 %, AAudio capture 2.0 %, playback 1.6 %, socket readers 1.4 %. |

### Not verified

Barge-in on the phone (the desktop harness showed it at ~200 ms), an unmuted conversation with audible reply and
echo behaviour, Bluetooth/wired headset routing, an AAudio device-disconnect restart, first-audio latency, and the
Wi-Fi → cellular handover. These need a person speaking, a headset, or a SIM.

### CPU: ideas not yet done

Fewer UDP reader wake-ups (one blocking read per socket instead of a 100 ms poll; wake them on close with a datagram
to themselves), binding sockets only for the interface that reaches OpenAI, and skipping the decode of 1–2 byte DTX
packets. Re-measure with a real battery test in Phase 7.

## Cost of the visuals (measured on the Moto g42, 2026-11-22, phone plugged in, screen on)

`am start --es visuals off` starts the app with no renderer, the baseline. Each row: fresh start, 12 s warm-up, 30 s window.
CPU from `/proc` ticks (device-wide and for the app), CPU frequency from a perfetto `power/cpu_frequency` trace (time-weighted,
% of each core's maximum), GPU time from `EXT_disjoint_timer_query` around each frame inside the app (the shell cannot read the
GPU's own counters).

| | visuals off | fluid | fractal |
|---|---|---|---|
| Frame rate | n/a | 60.0 fps (worst frame 33 ms) | 59.9 fps (worst frame 50 ms) |
| GPU time per frame | 0 | **12.5 ms = 75 %** of the 16.7 ms frame | **12.8 ms = 76 %** |
| Device CPU busy (all 8 cores) | 2.1 % | 5.5 % | 4.5 % |
| App CPU (% of one core) | 0.1 % | 15.6 % | 9.6 % |
| of which render thread | 0 | 12.8 % | 7.3 % |
| CPU frequency, little cores 0-3 (% of max) | 81 % | 80 % | 87 % |
| CPU frequency, big cores 4-7 (% of max) | 74 % | 74 % | 74 % |
| GPU temperature after the window | 33.6 C | 44.0 C (+1.6 during) | 43.2 C (+0.4 during) |

Reading it: the visuals cost little CPU and do not change the CPU clocks, but they keep the GPU about three quarters busy.
The two scenes cost the GPU the same; the fractal needs less CPU (no simulation bookkeeping). GPU frequency scaling could not be
observed (the kernel emitted no GPU frequency events during the window and its sysfs is blocked), so GPU load is given as time
per frame, not as a clock percentage. **Power/battery impact is not measured**: with the phone plugged in (and full) the battery
current says nothing about the load. It needs the phone unplugged (wireless adb) so the fuel gauge shows the real drain; a
reasonable guess from 75 % GPU load is a clearly visible drain, which is why adaptive quality and the option to cap the frame
rate or start at a lower level exist.

Other numbers from the same work: one fluid simulation step costs ~7.7 ms of GPU time (pre-roll: 180 steps in 1.38 s);
speed of the picture, as mean pixel change between screenshots, at a 1 s / 6 s gap: 7.2 / 19.2 (original) vs 2.5 / 4.6 (0.1x clock).
