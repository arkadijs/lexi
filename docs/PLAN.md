# Lexi — Implementation Plan for Claude CLI

How to use: run one phase per Claude CLI session (or a few small ones together). Start each session with:

> Read CLAUDE.md, docs/SPEC.md and docs/PLAN.md. Implement Phase N. Follow its tasks,
> make the "Done when" checks pass, tick the boxes in PLAN.md, and commit.

Phases are ordered so that the riskiest part (WebRTC to OpenAI) is proven on desktop
before any Android work. Phases 2 and 3 can run in parallel with Phase 4.

---

## Phase 0 — Repo skeleton and toolchain
- [x] Cargo workspace (`core`, `minter`, `devtools/desk`), `rust-toolchain.toml` (stable), shared lints.
- [x] `core` crate: `crate-type = ["cdylib", "rlib"]`, modules stubbed per SPEC §4.
- [x] Release profile per SPEC §8; `cargo deny` config; `.gitignore` (incl. `jniLibs/`, `*.keystore`, `.env`).
- [x] `scripts/setup-android-sdk.sh`: installs cmdline-tools packages via `sdkmanager`, prints env vars.
- [x] CI workflow: fmt, clippy, tests for the workspace. (not yet run in CI; unverified until pushed)

**Done when:** `cargo build --workspace` and `cargo test --workspace` pass; CI is green.

## Phase 1 — Minting service (`minter/`)
- [x] axum app with routes per SPEC §5.2, config from env, structured JSON logs, request IDs, timeouts.
- [x] OpenAI client for `/v1/realtime/client_secrets` (reqwest + rustls). **First verify the request/response shape against the current OpenAI API reference** and record it in `minter/README.md`.
- [x] Rate limiting (per install hash and per IP) with `governor`; 429 with `Retry-After`.
- [x] Attestation trait with `off` and `shared-key` implementations; `play-integrity` stubbed behind a feature.
- [x] `/healthz`, `/readyz`, `/metrics` (port 9090): request count, latency histogram, upstream errors, rate-limit hits.
- [x] Tests with `wiremock` for success, upstream 4xx/5xx, timeout, rate limit, bad attestation. Assert that tokens and keys never appear in logs.
- [x] Graceful shutdown.

**Done when:** tests pass; `OPENAI_API_KEY=... cargo run -p lexi-minter` and `curl -XPOST localhost:8080/v1/call-token` return a real `ek_` token.

## Phase 2 — Container + Kubernetes (`minter/Dockerfile`, `deploy/k8s/`)
- [x] Multi-stage Dockerfile: build static musl binary (cargo-chef for caching) → `distroless/static:nonroot`. Multi-arch (amd64, arm64) via buildx. (built and run on remote for amd64; the arm64/buildx multi-arch build is in CI only, not yet run)
- [x] Kustomize base + `dev`/`prod` overlays per SPEC §5.5. The Secret is referenced, not committed; document `kubectl create secret generic openai-api-key --from-literal=OPENAI_API_KEY=...`. (plus an `remote` overlay for the real cluster; see deploy/k8s/README.md)
- [x] `scripts/kind-up.sh`: create a kind cluster, load the image, apply the dev overlay, port-forward. (written, **not run**: no docker/kind available here; the dev overlay was only server-side dry-run on remote)
- [x] CI: build the image, `kustomize build | kubeconform -strict`, Trivy scan (fail on HIGH/CRITICAL). (jobs added to ci.yml, **not yet run in CI**)

**Done when:** on kind, the pod is Ready, runs as non-root with a read-only FS, and `curl` through the port-forward returns a token; image < 15 MB.

## Phase 3 — Desktop WebRTC proof (`core/rtc`, `core/openai`, `core/codec`, `devtools/desk`)
**The critical-risk phase.** Everything here must be platform-independent and reused unchanged on Android.
- [x] `openai` module: fetch token from the minter (`ureq` + rustls); SDP POST to `/v1/realtime/calls` **[verify headers/response in the current docs]**; event types for the data channel (serde, unknown events ignored).
- [x] `rtc` module: str0m with `rust-crypto` feature; add host candidates for all local interfaces; Opus-only audio m-line SendRecv + `oai-events` channel; net loop per SPEC §4.1, behind a `RtcSession` API: `start(token) -> handle`, `send_pcm_frame`, `on_pcm`, `on_event`, `close`.
- [x] `codec` module: libopus (`opus` or `audiopus` crate) 48 kHz mono, 20 ms, VOIP, FEC, DTX; correct RTP timestamps (+960 per frame).
- [x] `jitter` module: playout buffer with 60 ms target, reordering, PLC via Opus decode-with-loss, `flush()` for barge-in. Unit tests with simulated jitter/loss.
- [ ] `desk` CLI: `cpal` mic/speaker, prints events and levels, `--minter URL` or `--dev-key` (env only). _(headless live-tested; `cpal` streams verified to open at 48 kHz via `--check-audio`; a real mic/speaker conversation with a human is still to do)_
- [x] If DTLS with `rust-crypto` fails against OpenAI, switch str0m to `aws-lc-rs` and document why. _(not needed: DTLS works against OpenAI with `rust-crypto`)_

**Done when:** `cargo run -p desk -- --minter http://localhost:8080` lets you talk to Lexi on a laptop with headphones; barge-in works; Ctrl-C closes cleanly; the usage from `response.done` is printed.

## Phase 4 — Android shell (`android/`)
- [x] Gradle project (Kotlin, no Compose), minSdk 31, targetSdk current; R8 on; only `androidx.core` allowed.
- [x] Gradle task running `cargo ndk` before `preBuild` (SPEC §8); arm64 by default, `-PwithArmv7` flag. (verified by the build; armv7 untested)
- [x] `MainActivity`: full-screen `SurfaceView`, custom-drawn overlay buttons (Call, Mute, Route, Mode), status line, gesture detectors; edge-to-edge; permission flow. (code written; **not run on a device**)
- [x] `NativeBridge.kt` with the JNI surface from SPEC §4.2; `System.loadLibrary("lexi_core")`.
- [x] Rust `ffi` module: `jni` crate exports, `catch_unwind` on every entry, global `JavaVM`, callback dispatcher thread; `android_logger` for logcat. (compiled + clippy-clean for aarch64-linux-android; native calls are stubs)
- [x] `CallService` (FGS type microphone) + notification with Mute/Hang up actions; wake lock + Wi-Fi lock. (written, **not run on a device**)
- [x] `AudioRouter` per SPEC §6.3; `NetworkWatcher` → `onNetworkChanged()`. (written, **not run on a device**)
- [x] Install-ID: random UUID in SharedPreferences, sent as SHA-256 hex.

**Done when:** `./gradlew assembleDebug` produces an APK; on a device the buttons render and the permission flow works, with stubbed native calls logging to logcat.

## Phase 5 — Android audio + call integration (`core/audio`, wiring)
- [ ] AAudio input/output via `ndk::audio` with presets from SPEC §4.3; SPSC ring buffers (`rtrb`); restart streams on route change or error callback (disconnect).
- [ ] Resample to 48 kHz only if the device refuses 48 kHz.
- [ ] Wire `startCall` → minter → `RtcSession` → AAudio; state callbacks to Kotlin; mute via an atomic (send silence frames / DTX).
- [ ] Reconnect on network change (new token, backoff per SPEC §2.4); max-session guard.
- [ ] Level meters (RMS, smoothed) into atomics for the renderer.

**Done when:** acceptance criteria 1–5 from SPEC §10 pass on a real device.

## Phase 6 — GPU visuals (`core/render`)
- [ ] EGL setup on `ANativeWindow` (`khronos-egl`), GLES 3.0 via `glow`; render thread lifecycle tied to the surface callbacks; `AChoreographer` frame callbacks.
- [ ] Fluid: ping-pong FBOs (RG16F velocity, R16F pressure, RGBA8/RGBA16F dye; check `EXT_color_buffer_half_float` with a fallback), passes per SPEC §2.3, touch splats, audio-reactive uniforms.
- [ ] Fractal: smooth-coloured Mandelbrot/Julia, pan/pinch/double-tap, zoom clamp, palette animated by `bot_level`.
- [ ] Adaptive quality + frame-time stats in debug builds.
- [ ] Shaders embedded with `include_str!`; compile errors logged with source line numbers.

**Done when:** acceptance criterion 6 passes; no GL errors in logcat; rendering stops when locked (verify via logcat/systrace).

## Phase 7 — Hardening, size, release
- [ ] Measure and report sizes (`.so`, APK, AAB via `bundletool get-size total`); remove unused deps and features until N1 is met.
- [ ] Play Integrity attestation end to end (app side + minter `play-integrity` mode).
- [ ] Failure-mode tests from SPEC N5; battery check for 10 min of screen-off calling.
- [ ] Release signing config reading from env/CI secrets (no keystore in the repo); CI builds a signed AAB on tag.
- [ ] `README.md`: setup, build, run minter locally, deploy to k8s, configure the app's minter URL (`BuildConfig` field per build type).

**Done when:** all of SPEC §10 passes; CI produces a signed AAB and the minter image on a tag.

---

## Risk log (review at the start of each phase)
| Risk | Mitigation |
|---|---|
| str0m ↔ OpenAI interop (DTLS, SDP quirks) | Proven in Phase 3 on desktop first; crypto backend fallback; LiveKit `libwebrtc` bindings as a last resort (accepting +5–8 MB). |
| Echo on speakerphone with weak hardware AEC | VoiceCommunication preset; test on the device matrix; option to add WebRTC AEC3 (via the `webrtc-audio-processing` crate) behind a feature flag. |
| OpenAI API shape changes | All endpoints/fields are isolated in `core/openai` and `minter/src/openai.rs`; verify against the docs in Phases 1 and 3. |
| OEM background killing | Foreground service + wake locks + battery-optimisation prompt; test on Xiaomi/Samsung. |
| Token abuse / cost blow-up | Attestation, per-install and per-IP rate limits, short TTL, max session length, spend alerts on the OpenAI project. |
