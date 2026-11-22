# Lexi

A voice chat bot for Android: tap call, talk, and it talks back, in the language you speak, with web search for current
events. GPU fluid or fractal visuals react to the conversation. Two providers: **OpenAI Realtime** (WebRTC) and
**Google Gemini Live** (WebSocket), picked with the pills at the top of the screen. Sideloaded (no Play Store).

- `core/` Rust library (NDK): WebRTC and WebSocket transports, audio (AAudio), Opus, GPU renderer, call supervisor, prices.
- `android/` thin Kotlin shell (no dependencies): UI, foreground service, audio routing.
- `server/` Rust service `lexi` (Kubernetes): mints one short-lived token per call, runs OpenAI web search. No provider key ships in the app.
- `devtools/desk/` desktop harness: the same core with real audio or WAV files (`cargo run -p desk -- --help`).
- `deploy/k8s/` kustomize manifests; `scripts/` build, deploy and release tooling; `docs/` SPEC, PLAN and the RUNBOOK.

Start with `docs/SPEC.md` (what and why), `docs/PLAN.md` (status) and `docs/RUNBOOK.md` (operating it).

## Try it on a desktop

```
cargo test --workspace                                    # host tests
OPENAI_API_KEY=... cargo run -p lexi                      # the server on :8080 (add GEMINI_API_KEY=... for Google)
cargo run -p desk -- --minter http://localhost:8080 [--provider google] [--voice kore] --greet
```
Local builds use few cores on small machines: `cargo ... -j4`.

## Build the Android app

The Android toolchain (JDK, SDK, NDK, cargo-ndk) lives in a container image; on this project it is built and used on the
remote host: `scripts/android-remote.sh [gradle args]` (APK lands in `dist/`). Settings, as environment variables:

| Variable | Meaning |
|---|---|
| `LEXI_MINTER_URL` | server URL baked into the app (debug default `http://localhost:8080` for `adb reverse`; release default `https://lexi.example.com`) |
| `LEXI_ATTEST_KEY` / `LEXI_ATTEST_FROM_CLUSTER=1` | the attestation key baked into the app (one key, or the first key of the cluster Secret) |

Forgetting `LEXI_MINTER_URL` for a debug build gives an app that points at `localhost` and says "no network".

## Deploy the server

`scripts/deploy.sh` (details and the secrets in `docs/RUNBOOK.md`). Manifests: `deploy/k8s/` (`base` + overlays
`prod`, `remote`).

## Release for friends

`scripts/release-keystore.sh` once (back the key up!), then `scripts/release-android.sh` builds, aligns and signs
`dist/lexi-<version>.apk`; per-friend attestation keys and revoking one are in `docs/RUNBOOK.md`. `scripts/size-report.sh`
reports the size of the native library.
