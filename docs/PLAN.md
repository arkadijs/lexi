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
- [x] AAudio input/output via `ndk::audio` with presets from SPEC §4.3; SPSC ring buffers (`rtrb`); restart streams on route change or error callback (disconnect). (route-change restart verified on a device; the error-callback/disconnect restart and headsets are not)
- [x] Resample to 48 kHz only if the device refuses 48 kHz. (not implemented: the Moto g42 opens 48 kHz mono, and opening fails loudly otherwise)
- [x] Wire `startCall` → minter → `RtcSession` → AAudio; state callbacks to Kotlin; mute via an atomic (send silence frames / DTX). (muted calls verified on a device; an unmuted conversation was not)
- [x] Reconnect on network change (new token, backoff per SPEC §2.4); max-session guard. (unit-tested; verified on a device after a Wi-Fi drop and restore; the cellular handover is untestable without a SIM)
- [x] Level meters (RMS, smoothed) into atomics for the renderer. (atomics in `Shared`, unit-tested; nothing displays them until Phase 6)

**Done when:** acceptance criteria 1–5 from SPEC §10 pass on a real device.
*Status:* 1 (tap → media connected 1.2–2.35 s), 3 (muted call survived ~10 min screen-off; notification Hang up works) and
the speaker/earpiece part of 4 pass; the Wi-Fi-loss part of 5 passes (reconnect 1.3 s after Wi-Fi returned). Not yet
checked: 2 (barge-in on the phone), Bluetooth in 4, the cellular handover in 5. CPU with the screen off is 10 % against
N4's 5 % budget (see android/README.md).

## Phase 5c — Voice, language and latency (done 2026-11-22 except the open items)
- [x] Reply latency: found and fixed the cause of the long pause (DTX silence frames were not sent, so the server's VAD
  could not see the end of the user's turn): ~5.6 s → ~1.5 s; then `silence_duration_ms` 400 and `reasoning.effort` low
  (measured in SPEC §2.1; both are minter/ConfigMap settings, `minimal` effort is a documented option). The desktop harness has `--vad-silence-ms`, `--semantic-eagerness`, `--effort`, `--voice`,
  `--instructions` for dev-key measurements and prints `LATENCY end of user speech -> first bot audio`.
- [x] Minter prompt cleared and replaced: greet in Russian, expect Russian, follow a language switch; the core sends
  `response.create` once per call when the channel opens (SPEC §2.1).
- [x] Voice selector: minter `voice` allowlist (`LEXI_VOICES`), `startCall(…, voice)`, popup at the top of the screen,
  remembered; a change during a call applies to the next call. Narrowed after listening tests to **Cedar (default)** and
  **Alloy**, no section headers.
- [x] Default voice chosen by listening: Cedar (`LEXI_VOICE` in the ConfigMap, `Voices.kt`); Alloy as the alternative.
- [x] Talkativeness clamp: conversation-style prompt with hard length limits and a quiet-on-stop rule (SPEC §2.1);
  replies 3-8x shorter in the live-API A/B test.
- [ ] Verify on the phone: greeting, language switch, voice change, the new latency and the new prompt in a real conversation.

## Phase 5b — Running call cost (`core/pricing`, ffi, UI)
Show what the current call has cost so far (SPEC §2.6). The price table of `gpt-realtime-2.1-mini` is built into the app.
- [x] `core/pricing` (platform independent): the price table (USD per 1M tokens, with `VERIFIED_ON` and the source URL) and `cost_micro_usd(model, &Usage) -> Option<u64>`. Cached tokens are a **subset** of input tokens (bill uncached at the input rate and cached at the cached rate, per modality); unknown model → `None`. **Re-check the prices on the pricing page before building** (SPEC §2.6 lists what was verified on 2026-11-21).
- [x] Extend the `Usage` parsing in `core/openai/events.rs` with `image_tokens` and `cached_tokens_details` (`audio_tokens`, `text_tokens`, `image_tokens`); tolerate missing fields.
- [x] Supervisor: add each `response.done` usage to a per-call total (kept across reconnects), publish it in `Shared` (atomic micro-USD) and notify Kotlin; keep the existing usage log line (token counts only).
- [x] JNI: `NativeCallback.onCost(microUsd: Long)` (SPEC §4.2); reset to 0 on `startCall`.
- [x] UI: a one-line cost readout next to the status line, e.g. `$0.0123`; hidden before the first response; stays visible for 10 s after the call ends, then disappears; shown as tokens-only or hidden when the model has no known price.
- [x] Tests: pricing math against the documented example (253 tokens → $0.00205884) and edge cases (cached subset, image tokens, missing details, unknown model); supervisor test for accumulation across a reconnect; compare the displayed total with the OpenAI usage dashboard after a real call (they should agree to rounding). *(Only the last part is open: the on-device greeting call showed $0.0011 = 1099 uUSD, matching the logged tokens, but the dashboard comparison needs your account.)*

**Done when:** a real call shows a cost that grows after each reply and matches the usage dashboard for that call to within rounding.
*Status:* verified on the phone with the greeting response (screen = log). Please compare one call with the dashboard.

## Phase 6 — GPU visuals (`core/render`)
- [x] EGL setup on `ANativeWindow` (`khronos-egl`), GLES 3.0 via `glow`; render thread lifecycle tied to the surface callbacks; `AChoreographer` frame callbacks.
- [x] Fluid: ping-pong FBOs (RG16F velocity, R16F pressure, RGBA8/RGBA16F dye; check `EXT_color_buffer_half_float` with a fallback), passes per SPEC §2.3, touch splats, audio-reactive uniforms.
- [x] Fractal: smooth-coloured Mandelbrot/Julia, pan/pinch/double-tap, zoom clamp, palette shimmer and coordinate warp driven by `bot_level`.
- [x] Audio reactivity per SPEC §2.3: calm by default and while the user speaks (`mic_level` only nudges the calm motion); shimmer/distortion proportional to what the bot says (`bot_level` scales turbulence/dye brightness in the fluid and shimmer/warp in the fractal); fast-attack/slow-release smoothing; settles back to calm when the bot is silent. Check with recorded bot audio (desk `--out-wav`) and on the phone.
- [x] Adaptive quality + frame-time stats in debug builds.
- [x] Shaders embedded with `include_str!`; compile errors logged with source line numbers.

**Done when:** acceptance criterion 6 passes; no GL errors in logcat; rendering stops when locked (verify via logcat/systrace).

## Phase 6b — Model choice, Marin, web search, name (2026-11-22)
- [x] Marin voice in the pill between Cedar and Alloy; second pill Mini/Full; both disabled during a call; symmetric.
- [x] Minter: `model` in the token request (allowlist `LEXI_MODELS`), full-model prices in the core, cost rounded up to the cent.
- [x] Prompt checked against the full model (equally short replies; stop answered in under 2 s).
- [x] Web search: `web_search` function tool, minter `POST /v1/search` (Responses API), tool-call loop in the core, search cost in the total, verified end to end with the desk harness on both models (3 s search, 5-9 s spoken answer). The bot is named Lexi.
- [ ] Not verified: a spoken web-search conversation on the phone (adb cannot speak), the search cost estimate against the OpenAI dashboard.

## Phase 7 — Hardening, size, release
- [ ] Measure and report sizes (`.so` and the release APK); remove unused deps and features until N1 is met.
- [ ] Attestation runbook instead of Play Integrity (the app is sideloaded, not on Play): document key rotation (Secret → restart → rebuild); optionally accept several keys on the minter (per-friend keys). Delete the unused `play-integrity` stub.
- [ ] Failure-mode tests from SPEC N5; battery check for 10 min of screen-off calling.
- [ ] Release signing for sideloading: a keystore kept outside the repo (env/CI secret), `apksigner`-signed APK; CI builds it on tag and attaches it to a release. Keep the keystore backed up: a lost key means friends must uninstall before updating.
- [ ] `README.md`: setup, build, run minter locally, deploy to k8s, configure the app's minter URL (`BuildConfig` field per build type).

**Done when:** all of SPEC §10 passes; CI produces a signed APK and the minter image on a tag.

---

*Status (2026-11-22):* implemented and **verified on the Moto g42**: both scenes render at 60 fps with a call running; touch
(drag) stirs the fluid and pans the fractal; the bot's voice reaches the renderer and drives the shimmer (loudness is mapped
with a fixed x4 gain, as speech RMS peaks around 0.2); the renderer stops within milliseconds when the screen goes off (0 frames
logged while off) and restarts when it comes back; adaptive quality is in (never triggered at 60 fps). Cost is measured in
android/README.md ("Cost of the visuals"). Not verified: pinch-zoom (adb cannot synthesize two fingers), the shimmer judged
by eye during a real conversation, and the battery/power impact (needs the phone unplugged).
Choices made along the way: the fluid runs at 0.1x real time with a 3 s normal-speed pre-roll so it is developed at start; the
fractal is a Julia set whose constant travels the main cardioid of the Mandelbrot set (connected shapes only); the Mandelbrot
mode exists in the shader but has no UI toggle.

## Risk log (review at the start of each phase)
| Risk | Mitigation |
|---|---|
| str0m ↔ OpenAI interop (DTLS, SDP quirks) | Proven in Phase 3 on desktop first; crypto backend fallback; LiveKit `libwebrtc` bindings as a last resort (accepting +5–8 MB). |
| Echo on speakerphone with weak hardware AEC | VoiceCommunication preset; test on the device matrix; option to add WebRTC AEC3 (via the `webrtc-audio-processing` crate) behind a feature flag. |
| Prices change; the built-in table goes stale | `VERIFIED_ON` date next to the table; re-check on the pricing page whenever the model or `LEXI_MODEL` changes; the cost line is an estimate and is hidden for unknown models. Optional later: have the minter send the price table so a price change needs no app rebuild. |
| OpenAI API shape changes | All endpoints/fields are isolated in `core/openai` and `minter/src/openai.rs`; verify against the docs in Phases 1 and 3. |
| OEM background killing | Foreground service + wake locks + battery-optimisation prompt; test on Xiaomi/Samsung. |
| Token abuse / cost blow-up | Attestation, per-install and per-IP rate limits, short TTL, max session length, spend alerts on the OpenAI project. |
