# Lexi — Implementation Plan for Claude CLI

How to use: run one phase per Claude CLI session (or a few small ones together). Start each session with:

> Read CLAUDE.md, docs/SPEC.md and docs/PLAN.md. Implement Phase N. Follow its tasks,
> make the "Done when" checks pass, tick the boxes in PLAN.md, and commit.

Phases are ordered so that the riskiest part (WebRTC to OpenAI) is proven on desktop
before any Android work. Phases 2 and 3 can run in parallel with Phase 4.

---

## Phase 0 — Repo skeleton and toolchain
- [x] Cargo workspace (`core`, `service`, `devtools/desk`), `rust-toolchain.toml` (stable), shared lints.
- [x] `core` crate: `crate-type = ["cdylib", "rlib"]`, modules stubbed per SPEC §4.
- [x] Release profile per SPEC §8; `cargo deny` config; `.gitignore` (incl. `jniLibs/`, `*.keystore`, `.env`).
- [x] `scripts/setup-android-sdk.sh`: installs cmdline-tools packages via `sdkmanager`, prints env vars.
- [x] CI workflow: fmt, clippy, tests for the workspace. (not yet run in CI; unverified until pushed)

**Done when:** `cargo build --workspace` and `cargo test --workspace` pass; CI is green.

## Phase 1 — Minting service (`server/`)
- [x] axum app with routes per SPEC §5.2, config from env, structured JSON logs, request IDs, timeouts.
- [x] OpenAI client for `/v1/realtime/client_secrets` (reqwest + rustls). **First verify the request/response shape against the current OpenAI API reference** and record it in `server/README.md`.
- [x] Rate limiting (per install hash and per IP) with `governor`; 429 with `Retry-After`.
- [x] Attestation trait with `off` and `shared-key` implementations; `play-integrity` stubbed behind a feature.
- [x] `/healthz`, `/readyz`, `/metrics` (port 9090): request count, latency histogram, upstream errors, rate-limit hits.
- [x] Tests with `wiremock` for success, upstream 4xx/5xx, timeout, rate limit, bad attestation. Assert that tokens and keys never appear in logs.
- [x] Graceful shutdown.

**Done when:** tests pass; `OPENAI_API_KEY=... cargo run -p lexi` and `curl -XPOST localhost:8080/v1/call-token` return a real `ek_` token.

## Phase 2 — Container + Kubernetes (`server/Dockerfile`, `deploy/k8s/`)
- [x] Multi-stage Dockerfile: build static musl binary (cargo-chef for caching) → `distroless/static:nonroot`. Multi-arch (amd64, arm64) via buildx. (built and run on the cluster node for amd64; the arm64/buildx multi-arch build is in CI only, not yet run)
- [x] Kustomize base + `prod` overlay per SPEC §5.5 (a kind-oriented `dev` overlay existed and was removed on 2026-11-22: local development is `cargo run -p lexi`, the cluster is the deploy host). The Secret is referenced, not committed; document `kubectl create secret generic openai-api-key --from-literal=OPENAI_API_KEY=...`. (plus a `cluster` overlay for registry-less clusters; see deploy/k8s/README.md)
- [x] CI: build the image, `kustomize build | kubeconform -strict`, Trivy scan (fail on HIGH/CRITICAL). (jobs added to ci.yml, **not yet run in CI**)

**Done when:** on the cluster (the cluster), the pod is Ready, runs as non-root with a read-only FS, and `curl` through the ingress returns a token; image < 15 MB.

## Phase 3 — Desktop WebRTC proof (`core/rtc`, `core/openai`, `core/codec`, `devtools/desk`)
**The critical-risk phase.** Everything here must be platform-independent and reused unchanged on Android.
- [x] `openai` module: fetch token from the service (`ureq` + rustls); SDP POST to `/v1/realtime/calls` **[verify headers/response in the current docs]**; event types for the data channel (serde, unknown events ignored).
- [x] `rtc` module: str0m with `rust-crypto` feature; add host candidates for all local interfaces; Opus-only audio m-line SendRecv + `oai-events` channel; net loop per SPEC §4.1, behind a `RtcSession` API: `start(token) -> handle`, `send_pcm_frame`, `on_pcm`, `on_event`, `close`.
- [x] `codec` module: libopus (`opus` or `audiopus` crate) 48 kHz mono, 20 ms, VOIP, FEC, DTX; correct RTP timestamps (+960 per frame).
- [x] `jitter` module: playout buffer with 60 ms target, reordering, PLC via Opus decode-with-loss, `flush()` for barge-in. Unit tests with simulated jitter/loss.
- [ ] `desk` CLI: `cpal` mic/speaker, prints events and levels, `--service URL` or `--dev-key` (env only). _(headless live-tested; `cpal` streams verified to open at 48 kHz via `--check-audio`; a real mic/speaker conversation with a human is still to do)_
- [x] If DTLS with `rust-crypto` fails against OpenAI, switch str0m to `aws-lc-rs` and document why. _(not needed: DTLS works against OpenAI with `rust-crypto`)_

**Done when:** `cargo run -p desk -- --service http://localhost:8080` lets you talk to Lexi on a laptop with headphones; barge-in works; Ctrl-C closes cleanly; the usage from `response.done` is printed.

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
- [x] Wire `startCall` → service → `RtcSession` → AAudio; state callbacks to Kotlin; mute via an atomic (send silence frames / DTX). (muted calls verified on a device; an unmuted conversation was not)
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
  (measured in SPEC §2.1; both are server/ConfigMap settings, `minimal` effort is a documented option). The desktop harness has `--vad-silence-ms`, `--semantic-eagerness`, `--effort`, `--voice`,
  `--instructions` for dev-key measurements and prints `LATENCY end of user speech -> first bot audio`.
- [x] Service prompt cleared and replaced: greet in Russian, expect Russian, follow a language switch; the core sends
  `response.create` once per call when the channel opens (SPEC §2.1).
- [x] Voice selector: service `voice` allowlist (`LEXI_VOICES`), `startCall(…, voice)`, popup at the top of the screen,
  remembered; a change during a call applies to the next call. Narrowed after listening tests to **Cedar (default)** and
  **Alloy**, no section headers.
- [x] Default voice chosen by listening: Cedar (`LEXI_VOICE` in the ConfigMap, `Voices.kt`); Alloy as the alternative.
- [x] Talkativeness clamp: conversation-style prompt with hard length limits and a quiet-on-stop rule (SPEC §2.1);
  replies 3-8x shorter in the live-API A/B test.
- [x] Verified on the phone by the user (2026-11-22): greeting, voices, latency and the prompt in real conversations; the double greeting is gone.

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
- [x] Marin voice in the pill between Cedar and Alloy; second pill Mini/Smart; both disabled during a call; symmetric.
- [x] Service: `model` in the token request (allowlist `LEXI_MODELS`), full-model prices in the core, cost rounded up to the cent.
- [x] Prompt checked against the full model (equally short replies; stop answered in under 2 s).
- [x] Web search: `web_search` function tool, service `POST /v1/search` (Responses API), tool-call loop in the core, search cost in the total, verified end to end with the desk harness on both models (3 s search, 5-9 s spoken answer). The bot is named Lexi.
- [x] Spoken web search on the phone confirmed by the user (2026-11-22). Still open: the search cost estimate against the OpenAI dashboard.

## Phase 6c — Service renamed to `lexi`; gender and greeting rules (2026-11-22)
- [x] `lexi-service` → `lexi` (crate, `server/`, k8s, host `lexi.example.com`); old cluster resources deleted; app rebuilt for the new host.
- [x] Per-voice gender line appended by the server (`LEXI_VOICE_GENDERS`, Cedar male, Marin/Alloy female): "рад" vs "рада" verified on both models; "greet exactly once" added to the prompt.
- [x] Model label Full → Smart.

## Phase 6d — Google Gemini Live as a second provider (2026-11-22)
Verified with the user's keys: `models/gemini-3.8-live`; `POST /v1beta/auth_tokens` accepts `bidiGenerateContentSetup` (locks model, voice, instructions, tools; the documented `liveConnectConstraints` is rejected); Live is WebSocket only; function calling works; usage is per generation.
- [x] Server: `provider` in the token request, `GOOGLE_API_KEY` (Secret `google-api-key`, optional in the deployment, created by `scripts/deploy.sh` when the variable is set), locked setup with prompt + gender line + `web_search` function, `LEXI_GEMINI_*` settings, tests with a mock Google.
- [x] Search: Gemini uses Google's built-in `googleSearch` inside the Live session (queries billed per query); OpenAI keeps its own search through the server (providers compartmentalized, decided with the user). Measured on the same question: built-in 1.6 s to the spoken answer; function route through OpenAI 8.3 s search + 15.8 s total.
- [x] Core: `core/src/gemini` WebSocket session (tungstenite + rustls/ring + webpki-roots, no new crypto stack), 48↔16/24 kHz resampling, paced playout, barge-in, usage → cost, per-query search cost, prices in `pricing`; 8 tests including a full scripted conversation.
- [x] App: provider pill left of the voice pill (OpenAI | Google), voice list per provider, model pill fixed to "Live" for Google, all locked during a call.
- [x] Verified live: desk harness and the phone (live in 1.4 s, greeting, cost); Charon says "рад", Kore "рада"; web search answered by voice (search 3.4 s).
- [x] Voice samples: `Download/lexi-voices/gemini-<voice>-ru|en.wav` on the phone (Google's TTS model as a proxy; rate-limited, partial).
- [x] The offered Gemini voices (Charon, Fenrir, Kore, Leda) were confirmed by the user (2026-11-22).
- [x] Session length: Live audio sessions end after ~15 minutes; accepted by the user, no resumption will be built.
- [ ] Open: `thinkingLevel` (untested), Gemini price check against a real bill. (The funded `GOOGLE_API_KEY` is deployed; both providers confirmed working on the phone by the user.)

## Phase 7 — Hardening, size, release
- [x] Sizes (2026-11-22, `scripts/size-report.sh`): the stripped `.so` was 5.55 MB at the start of the phase and is **5.01 MB** now (target relaxed to 6 MB on 2026-11-22: **met**); the signed release APK is **5.09 MB** (target 6 MB: met). What helped: one TLS crypto stack instead of two (rustls and ureq on aws-lc, which str0m's DTLS already links; `ring` gone, -310 KB) and linker flags in `.cargo/config.toml` (`--icf=all`, `--pack-dyn-relocs=relr`, -222 KB). What is left is mostly dependencies we need: core/alloc/std ~640 KB, str0m + dimpl + sctp ~520 KB, `combine` (SDP parser) 205 KB, rustls 160 KB, the C crypto of aws-lc ~430 KB, Opus ~160 KB. Reaching 4 MB would have meant replacing str0m or the crypto, which is not worth it for a sideloaded app: **N1 relaxed to ".so < 6 MB, APK < 6 MB"** by the user (SPEC updated).
- [x] Attestation runbook instead of Play Integrity: `docs/RUNBOOK.md` (add/revoke a friend's key, rotation, provider keys); the server accepts several comma-separated keys (`LEXI_ATTEST_SHARED_KEY`), tested; the `play-integrity` stub is deleted.
- [x] Failure-mode tests from SPEC N5 on the phone (`docs/FAILURE-MODES.md`): 14 scenarios, 2 bugs found and fixed (reconnect gave up after ~4 s; a wrong phone clock was reported as an auth error). Wired headset and output-device transitions were verified by the user; still untested: Bluetooth headsets and phone-call audio focus.
- [x] Battery power measured unplugged over wireless adb (`scripts/power-test.sh`, results and estimates in `docs/POWER.md`): the fluid costs +1.06 W, the fractal +0.78 W, a muted call +0.35-0.45 W; a screen-on call lasts ~9 h with the fluid, ~17 h without visuals. **Not resolved:** the screen-off call (the method has a ~0.5-0.7 W floor, so it is only bounded above by 0.7 W); needs a 30-60 min run without adb.
- [~] Release signing for sideloading: `scripts/release-keystore.sh` (key in `~/.config/lexi`, outside the repo) and `scripts/release-android.sh` (build, zipalign, apksigner on the build host, key over ssh stdin) work; the first signed APK `dist/lexi-0.1.0.apk` was built and installed on the phone (cert SHA-256 a4724756...3596). The user must back up `~/.config/lexi/release.{jks,pass}`. **Not done:** CI building it on a tag (the workflows have never run: no remote), and a spoken call on the release build (a Google sign-in prompt covered the phone's screen during the check).
- [x] `README.md`: setup, build, run the server locally, deploy, configure the app's server URL.

**Done when:** all of SPEC §10 passes; CI produces a signed APK and the service image on a tag.

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

## Phase 8 — Preparing the public repository (2026-11-22)
- [x] History scanned for credentials: the real provider keys, the attestation keys and the release password appear in none of the 49 commits (searched by exact value), no `.envrc`/keystore/`.pem` file was ever committed, and a pattern scan found only test fixtures (`sk-test…`) and checksums. Left to the owner: the commit author name and e-mail are public once pushed.
- [x] `.envrc.example` (provider keys, `LEXI_DEPLOY_HOST`, the derived `LEXI_SERVICE_HOST`/`LEXI_SERVICE_URL`, optional node/issuer/ingress settings); every script reads `.envrc` through `scripts/env.sh`; no cluster host name is left in the tree (the ingress host is a placeholder filled in at deploy time).
- [x] `LEXI_MINTER_URL` → `LEXI_SERVICE_URL`; "minter" renamed to "service" in code, scripts and docs; the deploy and Android build scripts and the kustomize overlay lost their host-specific names (`deploy.sh`, `android-remote.sh`, overlay `cluster`).
- [x] `GOOGLE_API_KEY` everywhere (Secret `google-api-key`); both provider keys are optional: a provider without a key is rejected with HTTP 501, the app shows "This provider is not set up on the server" and never retries; with no key the service still starts and the deploy script only warns.
- [x] README with screenshots and prerequisites (provider keys incl. Google prepay, Kubernetes access and cluster requirements, build host, phone); BSD 2-Clause `LICENSE`; the CI workflow was removed (no CI for now).
- [x] The NetworkPolicy moved out of `base/` into `deploy/k8s/optional/networkpolicy.yaml`: off by default (it names cluster-specific things: the ingress controller's namespace, `monitoring`, CoreDNS), enabled with `LEXI_NETWORK_POLICY=on`; documented in the README and `deploy/k8s/README.md`. The owner's own deployment keeps it on through `.envrc`.
- [x] The default bot prompt is Russian-oriented: documented how to change it (README "Changing the bot's prompt", RUNBOOK "Prompt pieces"); `LEXI_PROMPT_FILE` in `.envrc` deploys a different prompt without touching the repository copy; `deploy/k8s/prompts/english.txt` is an English starting point; the desktop harness's default prompt no longer says "vocabulary coach".
- [x] Branch renamed `master` → `main`.
- [x] The commit author identity and the `Co-Authored-By` trailers were kept as they are (owner's decision); `main` was pushed to `origin` (2026-11-22).

## Risk log (review at the start of each phase)
| Risk | Mitigation |
|---|---|
| str0m ↔ OpenAI interop (DTLS, SDP quirks) | Proven in Phase 3 on desktop first; crypto backend fallback; LiveKit `libwebrtc` bindings as a last resort (accepting +5–8 MB). |
| Echo on speakerphone with weak hardware AEC | VoiceCommunication preset; test on the device matrix; option to add WebRTC AEC3 (via the `webrtc-audio-processing` crate) behind a feature flag. |
| Prices change; the built-in table goes stale | `VERIFIED_ON` date next to the table; re-check on the pricing page whenever the model or `LEXI_MODEL` changes; the cost line is an estimate and is hidden for unknown models. Optional later: have the service send the price table so a price change needs no app rebuild. |
| OpenAI API shape changes | All endpoints/fields are isolated in `core/openai` and `server/src/openai.rs`; verify against the docs in Phases 1 and 3. |
| OEM background killing | Foreground service + wake locks + battery-optimisation prompt; test on Xiaomi/Samsung. |
| Token abuse / cost blow-up | Attestation, per-install and per-IP rate limits, short TTL, max session length, spend alerts on the OpenAI project. |
