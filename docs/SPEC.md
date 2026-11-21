# Lexi — Product & Technical Specification

> Codename **Lexi** (rename freely). A voice-first Android app for building word acumen:
> the user "calls" an AI vocabulary coach and talks to it, while the single screen shows
> GPU-rendered fluid or fractal visuals that react to the conversation.

Status: v1 spec. Items marked **[VERIFY]** depend on external APIs that change often — check
the current official docs before implementing them.

---

## 1. Goals and non-goals

### Goals
- G1. Voice conversation with an AI coach over **WebRTC** directly to the **OpenAI Realtime API**
  (model `gpt-realtime-2.1-mini`).
- G2. Audio works on the **loudspeaker, earpiece and wired/Bluetooth earbuds**, user-switchable.
- G3. The call **keeps working when the phone is locked** or the app is in the background.
- G4. One screen: full-screen **GPU visuals** (Navier–Stokes fluid or fractal explorer) + a few custom buttons.
- G5. **Small and efficient**: Rust core via NDK, thin Kotlin shell, no heavy frameworks.
- G6. The OpenAI API key never ships in the app. A **Rust token-minting service on Kubernetes**
  issues one short-lived ephemeral key per call.

### Non-goals (v1)
- iOS, tablets-specific layouts, wear OS.
- Chat history, accounts, transcripts UI, text chat.
- Video, screen sharing, multi-party calls.
- Offline mode.
- Telecom/ConnectionService integration (planned for v1.1, see §6.4).
- Publishing on Google Play (and therefore Play Integrity, AABs and Play size accounting). The app is sideloaded
  as a signed APK for the author and friends only; distribution is a direct file/release link.

---

## 2. Product behaviour

### 2.1 The coach
- **Prompt** (cleared and rewritten 2026-11-22): language behaviour only, no persona or role. The model greets the
  user with one short sentence in Russian before they say anything, expects a Russian-speaking user and answers in
  Russian by default, switches language when the user does (or asks it to) and stays in it until they switch again,
  and speaks naturally with native pronunciation in whatever language it uses. The text is a minter config value
  (`LEXI_INSTRUCTIONS_FILE`, the `lexi-minter-instructions` ConfigMap), so it changes without an app release.
- **Greeting trigger:** instructions alone do not make the model speak first. The core sends `response.create` once
  when the `oai-events` channel opens (once per call: a reconnect does not greet again). It costs a few audio tokens.
- Brevity and the vocabulary-coach role are no longer instructed; replies can be longer. The cost readout (§2.6)
  shows what that costs.
- Instructions, voice, turn detection and reasoning effort are set **server-side by the minter** when the ephemeral
  key is created; the app only chooses the voice (below).
- **Voice:** selectable from a popup at the top of the screen (§2.2) with two entries, **Cedar** (the default) and
  **Alloy**, no section headers. The minter accepts all ten built-in voices (`LEXI_VOICES`); only the app's menu is
  narrowed. The choice is sent as `voice` in the token request (§5.2), validated by the minter, remembered across
  launches (a stored voice that is no longer offered falls back to Cedar) and **fixed for the whole call**: the API
  locks the voice once the model has spoken, so a change made during a call applies to the next one.
- **Russian voice quality:** there is no official rating. OpenAI's docs call the voices "optimized for English" (TTS
  page), recommend Marin and Cedar for best quality (not language-specific), and say language and accent are controlled
  separately in the prompt. A community thread (community.openai.com, "Realtime Russian voice…") reports an
  English-like melody, unstable consonants and a metallic resonance in Russian, and custom voices need enterprise
  approval. Choosing a voice for Russian is therefore empirical.
  Measured 2026-11-22: all ten voices read the same Russian paragraph (desktop harness, dev key), transcribed with
  `gpt-4o-transcribe` (language ru). Eight voices scored 0 % word errors, so speech-to-text cannot rank them
  (it measures intelligibility, not naturalness); `ballad` scored 12.5 % and `verse` read only the first sentence of the
  text (56 % errors: it does not follow "read exactly" well). Naturalness has to be judged by ear: the ten samples
  were copied to the test phone (`Download/lexi-voices/ru-<voice>.wav`) and the voices are selectable in the app.
- **Turn latency** (measured 2026-11-22, desktop harness against the live API, end of user speech → first bot audio):
  the first version took ~5.6 s because DTX silence frames were not sent (§4.3). After fixing that: server VAD 500 ms +
  default reasoning ≈ 1.47 s; `reasoning.effort=minimal` ≈ 1.29 s; silence 300 ms ≈ 1.41 s; both ≈ 1.19 s; semantic VAD
  "high" ≈ 1.77 s (slower). Defaults chosen: server VAD with `silence_duration_ms` 400 and `reasoning.effort` **low**
  (the API default; `minimal` is ~0.2 s faster but was not adopted). Both are minter settings, set in its ConfigMap and
  changeable without an app release: `LEXI_VAD_SILENCE_MS`, `LEXI_REASONING_EFFORT`.

### 2.2 Screen (single Activity, portrait + landscape)
- Full-screen `SurfaceView` with the visual.
- Overlay buttons (native Android `View`s, custom-drawn, large touch targets, content descriptions for accessibility):
  1. **Call / Hang up** (primary, centred bottom). Shows state: idle → connecting → live → ending.
  2. **Mute mic** (toggle). Works before the call too: muting first makes the call start muted
     (the mic stays silent from the first frame).
  3. **Audio route** (cycles available outputs: Earpiece → Speaker → Bluetooth/Wired, showing only available ones; icon reflects current route).
  4. **Visual mode** (Fluid ↔ Fractal).
- Small status line (one text line) for errors such as "No network" or "Microphone permission needed".
- A one-line **cost readout** for the current call (see §2.6).
- A **voice selector** at the top of the screen (the bottom is already busy): a pill showing the current voice that
  opens a popup menu with the selectable voices (§2.1). The status line sits under it.
- Gestures:
  - Fluid mode: touch/drag injects force and dye.
  - Fractal mode: drag to pan, pinch to zoom, double-tap to reset.

### 2.3 Visuals
- **Fluid**: Stable Fluids (Jos Stam) on the GPU: advection, divergence, Jacobi pressure
  solve (≈20 iterations), gradient subtraction, dye advection, curl/vorticity confinement.
  Simulation grid at ≤ half screen resolution, dye at screen resolution or ¾.
  Renders as flowing wave-like lines/colour bands.
- **Fractal**: Mandelbrot/Julia in one fragment shader with smooth colouring; fp32 precision,
  so zoom is clamped (~1e5). Deep zoom (perturbation) is out of scope for v1.
- **Audio reactivity**: two values passed in as shader uniforms each frame:
  `mic_level` and `bot_level` (smoothed RMS, 0..1). The visuals are **calm by default**: when nothing is said, and
  while the *user* speaks, they animate gently (slow flow, slow colour drift; `mic_level` may only nudge that calm
  motion a little). They **shimmer and distort in proportion to what the bot says**: `bot_level` scales the amplitude
  of the shimmer/distortion (fluid: turbulence, vorticity and dye brightness; fractal: palette shimmer and a warp of
  the coordinates). Silence from the bot settles them back to calm. Use fast-attack/slow-release smoothing so the
  motion follows the voice without flicker.
- Frame pacing via `AChoreographer`; target 60 fps, adaptive quality (drop sim resolution
  if frame time > 14 ms for 1 s).
- Rendering **stops entirely** when the surface is destroyed or the Activity is not visible.

### 2.4 Call lifecycle
1. User taps Call → app requests `RECORD_AUDIO` (and `POST_NOTIFICATIONS` on Android 13+) if missing.
2. App starts the foreground service **while in the foreground**, then the service:
   fetches an ephemeral key from the minter → builds the WebRTC offer → POSTs it to OpenAI
   → applies the answer → audio flows.
3. The call continues with the screen off or locked. An ongoing notification shows
   "Talking with Lexi", with **Mute** and **Hang up** actions.
4. The call ends on: Hang up (UI or notification), remote close, fatal error, audio focus
   loss to a phone call (see §6.3), or the max-session guard (configurable, default 30 min).
5. Network change (Wi-Fi ↔ cellular) → automatic reconnect with a **new** ephemeral key
   (max 3 attempts, backoff 0.5 s/1 s/2 s). The conversation context is lost on reconnect in v1.

### 2.6 Running call cost
The app shows what the current call has cost so far. The server reports exactly what it bills in the `usage` of each
`response.done` event ("Usage statistics for the Response, this will correspond to billing"), so the figure is the sum
of those usages priced with a **price table built into the app**.

- **Prices** (USD per 1M tokens, `gpt-realtime-2.1-mini`, Realtime API, standard tier; verified 2026-11-21 on
  <https://developers.openai.com/api/docs/pricing>, "Realtime & Audio"):

  | Modality | Input | Cached input | Output |
  |---|---|---|---|
  | Audio | $10.00 | $0.30 | $20.00 |
  | Text | $0.60 | $0.06 | $2.40 |
  | Image | $0.80 | $0.08 | not listed |

  Not in the table, deliberately: batch/flex/priority tiers (none are listed for this model) and the 10 % regional
  processing uplift (we use the default endpoint). Input audio transcription is billed separately by OpenAI, so it
  stays **off** (it is off by default); enabling it would make the readout too low.
- **Formula** per `response.done`: `usage.input_token_details` holds the totals per modality and
  `cached_tokens_details` the cached **subset** of each, so
  `cost = Σ_modality (total − cached) × input_rate + cached × cached_rate + Σ output_tokens × output_rate`
  (output has only text and audio). Worked example from OpenAI's cost guide (text in 119 of which 64 cached, audio in 13,
  text out 30, audio out 91): 55 × 0.60 + 64 × 0.06 + 13 × 10 + 30 × 2.40 + 91 × 20 = 2058.84 µ$ ≈ $0.00206.
- **Granularity:** usage only arrives when a response completes, so the readout steps up after each reply. Listening
  is free until a response is created: VAD filters empty audio, and user audio is 1 token per 100 ms, assistant audio 1
  token per 50 ms.
- **Per call, across reconnects:** the total keeps accumulating if the call reconnects (billing does too), and resets
  when a new call starts. It stays on screen after hang-up until the next call.
- **Unknown model:** the minter returns the model it used. If it is not in the built-in table (for example a changed
  `LEXI_MODEL`), the readout is hidden rather than wrong.
- **Display:** US dollars, four decimals below $1 (`$0.0123`), two above. An estimate to be compared with the OpenAI
  usage dashboard; the table carries a `VERIFIED_ON` date and must be re-checked when prices or the model change.

---

## 3. Non-functional requirements

| ID | Requirement |
|---|---|
| N1 | **Size**: release APK < **6 MB** (arm64-v8a). Rust `.so` < 4 MB stripped. No AndroidX Compose, no Kotlin coroutines library unless justified; only `androidx.core` allowed. |
| N2 | **minSdk 31** (Android 12; needed for `setCommunicationDevice`), **targetSdk** 36 (the current Play requirement, verified 2026-11-21; not mandatory for a sideloaded app, but there is no reason to run in older compatibility modes). ABIs: `arm64-v8a` (required), `armeabi-v7a` (optional build flag). |
| N3 | **Latency**: tap-to-first-audio < 2.5 s on 4G (excluding model think time). Mouth-to-ear added by the client < 80 ms (20 ms frames + ≤ 60 ms playout buffer). |
| N4 | **CPU/battery**: during a call with the screen off, app CPU < 5 % of one big core on a mid-range device; no rendering, no busy loops. |
| N5 | **Robustness**: no crash on permission denial, airplane mode, BT headset connect/disconnect mid-call, or the minter being unreachable. Rust panics must not cross FFI (`catch_unwind` at every JNI entry point); `panic = "abort"` only in release, after FFI guards. |
| N6 | **Security**: no OpenAI key in the APK or the repo. The ephemeral key is held only in memory and never logged. TLS everywhere (rustls). |
| N7 | **Privacy**: no audio stored on device or server. The minter logs no PII; the install ID is hashed before being sent as the OpenAI safety identifier. |

---

## 4. Architecture

```
┌──────────────────────── Android app ────────────────────────┐
│ Kotlin shell (≈ 400–600 LOC)                                │
│  MainActivity ─ SurfaceView + overlay buttons + gestures    │
│  CallService (foreground, type=microphone)                  │
│  AudioRouter (AudioManager comm devices, focus, BT events)  │
│  NetworkWatcher (ConnectivityManager callback)              │
│  NativeBridge (JNI externs + callbacks)                     │
└──────────────┬──────────────────────────────────────────────┘
               │ JNI (small, stable C-like surface)
┌──────────────▼──────── Rust core: liblexi_core.so ──────────┐
│ ffi/      JNI entry points, panic guards, callback dispatch │
│ render/   EGL + GLES3 (glow): fluid + fractal, choreographer │
│ audio/    AAudio in/out (VoiceCommunication), ring buffers   │
│ codec/    libopus encode/decode 48 kHz mono 20 ms            │
│ rtc/      str0m session, UDP loop, SDP, data channel         │
│ openai/   token fetch (minter), SDP exchange, event JSON     │
│ jitter/   playout buffer (60 ms target, PLC on loss)         │
│ state/    call state machine, shared atomics (levels, mute)  │
└──────────────┬──────────────────────────────────────────────┘
               │ HTTPS
   ┌───────────▼───────────┐        ┌─────────────────────────┐
   │ lexi-minter (Rust,    │ HTTPS  │ OpenAI                  │
   │ axum, Kubernetes)     ├───────►│ POST /v1/realtime/      │
   └───────────────────────┘        │      client_secrets     │
   app ──── WebRTC (UDP, DTLS-SRTP + SCTP) ───► /v1/realtime/calls
```

### 4.1 Threads (Rust core)
- **net thread**: owns `str0m::Rtc` and the UDP socket. Loop: `poll_output` → send /
  timeout → `recv_from` with the deadline → `handle_input`. Also does Opus encode (reads mic
  ring buffer every 20 ms) and Opus decode (writes PCM into the playout buffer).
- **AAudio input callback**: copies PCM into an SPSC ring buffer (`rtrb`). No allocation, no locks.
- **AAudio output callback**: pulls from the jitter/playout buffer; outputs silence on underrun.
- **render thread**: EGL context bound to the `ANativeWindow`; runs only while the surface exists.
- Shared state via atomics: `mic_level`, `bot_level`, `muted`, `call_state`.
- Kotlin callbacks (state changes, errors) are delivered from a dedicated, JNI-attached
  dispatcher thread using a bounded channel. Never call into the JVM from audio callbacks.

### 4.2 JNI surface (Kotlin `NativeBridge` ↔ Rust)
```
init(context, callback: NativeCallback)               // once, from Application
surfaceCreated(surface), surfaceChanged(w, h), surfaceDestroyed()
setVisualMode(mode: Int), touch(action, pointerId, x, y), pinch(scale, fx, fy)
startCall(minterUrl: String, installIdHash: String, attestKey: String)   // runs async, reports via callback;
                                                      // attestKey = build-time shared key ("" = none), see §5.4
endCall()
setMuted(muted: Boolean)
onAudioRouteChanged()                                 // restart AAudio streams if needed
onNetworkChanged()                                    // triggers reconnect if in call
NativeCallback.onState(state: Int, detail: String?)   // Idle/Connecting/Live/Reconnecting/Ended/Error
NativeCallback.onCost(microUsd: Long)                 // running cost of the current call, USD x 1e-6 (see §2.6)
```

### 4.3 WebRTC / OpenAI specifics
- Library: **`str0m`** (sans-I/O). Crypto feature: `rust-crypto` (pure Rust, easy NDK
  cross-compile); fall back to `aws-lc-rs` if interop fails.
- Offer: one audio m-line, `SendRecv`, Opus only, plus a data channel labelled **`oai-events`**.
- ICE: host candidates for each active local interface (IPv4 + IPv6). Add a STUN
  server-reflexive candidate only if testing shows failures **[VERIFY]**.
- SDP exchange: `POST https://api.openai.com/v1/realtime/calls`,
  `Authorization: Bearer <ephemeral ek_...>`, `Content-Type: application/sdp`, body = offer;
  response body = answer SDP **[VERIFY]**.
- Data-channel events handled in v1:
  - `session.created` / `session.updated` → log only.
  - `input_audio_buffer.speech_started` → **flush the local playout buffer** (barge-in).
  - `response.done` → add `usage` to the running call cost (§2.6) and log the token counts (never content).
  - `error` → surface to UI and end the call if fatal.
- Audio: capture 48 kHz mono, `AAUDIO_INPUT_PRESET_VOICE_COMMUNICATION`,
  `AAUDIO_USAGE_VOICE_COMMUNICATION`, `AAUDIO_CONTENT_TYPE_SPEECH`, performance mode
  low-latency. If the device gives a different rate, resample (`rubato`) to 48 kHz.
- Opus encoder: VOIP application, 32 kbps, complexity 5, in-band FEC on, DTX on. **DTX frames are still sent**:
  while the user is silent the encoder emits 1–2 byte "silence" frames, and withholding them made the server's VAD
  wait seconds to notice the end of the user's turn (reply latency 5.6 s instead of 1.5 s). Sent, they cost ~1 kB/s.
  While muted nothing is encoded or sent (the RTP clock keeps running).

---

## 5. Token-minting service — `lexi-minter`

### 5.1 Purpose
Holds the OpenAI API key. Issues **one ephemeral client secret per call**, with the session
configuration (model, instructions, voice, turn detection) fixed server-side.

### 5.2 API
```
POST /v1/call-token
  Headers: X-Lexi-Install: <sha256(install-id) hex>
           X-Lexi-Attest: <app attestation token>          (see 5.4)
  Body: {}   (optional: {"voice":"cedar","locale":"en"}; voice must be one of LEXI_VOICES)
  200 → {"token":"ek_...","expires_at":<unix>,"model":"gpt-realtime-2.1-mini"}
  400 invalid body / unknown voice · 401 attestation failed · 429 rate limited (Retry-After) ·
  502 upstream error · 503 not ready
GET /healthz   (liveness, no dependencies)
GET /readyz    (ready once config is loaded and the key is present)
GET /metrics   (Prometheus; on a separate port 9090)
```

Upstream call **[VERIFY exact body against the current API reference]**:
`POST https://api.openai.com/v1/realtime/client_secrets` with `Authorization: Bearer $OPENAI_API_KEY`,
header `OpenAI-Safety-Identifier: <install hash>`, body containing `expires_after`
(shortest practical TTL, default 60 s, just long enough to complete the SDP exchange) and `session`
`{type:"realtime", model, instructions, audio.output.voice, audio.input.turn_detection}`.

### 5.3 Implementation
- Rust, `tokio` + `axum`, `reqwest` (rustls, no OpenSSL), `serde`, `tracing` + JSON logs,
  `metrics` + `metrics-exporter-prometheus`, `tower-http` (timeouts, request IDs, body limit).
- Config from env (12-factor): `OPENAI_API_KEY` (from a Secret), `LEXI_MODEL`,
  `LEXI_VOICE` (default), `LEXI_VOICES` (allowed list), `LEXI_REASONING_EFFORT`, `LEXI_VAD_SILENCE_MS`,
  `LEXI_INSTRUCTIONS_FILE` (mounted ConfigMap), `LEXI_TOKEN_TTL_SECS`,
  `LEXI_RATE_PER_INSTALL` (default 30/hour), `LEXI_RATE_PER_IP` (default 120/hour),
  `LEXI_ATTEST_MODE` (`off` | `shared-key` | `play-integrity`).
- Rate limiting: in-memory per replica in v1 (`governor`); optional Redis backend behind a feature flag.
- Never log tokens, the API key, or the raw install ID. Redact the `Authorization` header in traces.
- Graceful shutdown on SIGTERM (drain ≤ 10 s). Upstream timeout 5 s, one retry on 5xx/connect errors.
- Image: static `x86_64-unknown-linux-musl` (and `aarch64` for ARM nodes) binary in
  `gcr.io/distroless/static:nonroot` or `scratch`. Target image < 15 MB.

### 5.4 Client attestation (abuse protection)
- `off`: development only.
- `shared-key`: the app sends `X-Lexi-Attest: <unix-ts>.<hex(hmac_sha256(key, ts))>` (±300 s skew) with a
  build-time key, computed fresh for every token request (so reconnects never reuse a stale timestamp). The core
  computes it from the `attestKey` passed to `startCall`. Weak (the key ships in the app), MVP only.
- `shared-key` is the **production mode** for this project: the app is sideloaded for the author and a few
  friends, so a leaked key is a manageable event. Rotation: replace the `lexi-attest-key` Secret, restart the
  minter, rebuild the APK; old builds then get 401 until updated. If one friend's copy leaks or someone leaves,
  per-friend keys (several accepted keys on the minter) are the next step.
- `play-integrity`: **not planned** (it only works for apps distributed through Google Play). The stub behind the
  `play-integrity` cargo feature is unused and may be deleted.

### 5.5 Kubernetes
- Manifests with **kustomize** in `deploy/k8s/` (`base/` + `overlays/dev`, `overlays/prod`):
  Deployment (2+ replicas, `runAsNonRoot`, read-only root FS, all capabilities dropped,
  seccomp `RuntimeDefault`, requests 20m CPU / 32Mi, limits 200m / 64Mi), Service,
  Ingress (TLS via cert-manager annotation, host configurable), HPA (CPU 70 %),
  PodDisruptionBudget (minAvailable 1), ConfigMap (instructions + non-secret config),
  Secret reference (`openai-api-key`, created out-of-band or via External Secrets; **never committed**),
  NetworkPolicy (ingress from the ingress controller only; egress DNS + 443), ServiceMonitor (optional).
- Probes: liveness `/healthz`, readiness `/readyz`, startup probe.
- CI builds the multi-arch image and runs `kubeconform` on the rendered manifests.

---

## 6. Android platform details

### 6.1 Manifest
Permissions: `RECORD_AUDIO`, `INTERNET`, `ACCESS_NETWORK_STATE`, `FOREGROUND_SERVICE`,
`FOREGROUND_SERVICE_MICROPHONE`, `POST_NOTIFICATIONS`, `WAKE_LOCK`, `MODIFY_AUDIO_SETTINGS`,
`BLUETOOTH_CONNECT` (to list BT devices on 12+).
Service: `CallService` with `android:foregroundServiceType="microphone"`.

### 6.2 Background / lock screen
- Start the foreground service only from a visible Activity (Android 14+ blocks starting a
  microphone FGS from the background).
- During a call: `PARTIAL_WAKE_LOCK` and `WifiManager` low-latency lock; release both on end.
- Ask once (with explanation) to ignore battery optimisation if the device's manufacturer is
  known to be aggressive (Xiaomi, Huawei, Oppo, Vivo, Samsung).

### 6.3 Audio routing and focus
- On call start: `AudioManager.mode = MODE_IN_COMMUNICATION`; request audio focus
  `AUDIOFOCUS_GAIN` with `USAGE_VOICE_COMMUNICATION`.
- Route selection via `availableCommunicationDevices` / `setCommunicationDevice`.
  Default priority: Bluetooth > wired headset > **speaker** (the coach is used hands-free) >
  earpiece. Remember the last user choice.
- React to device add/remove (`AudioDeviceCallback`): auto-switch to a newly connected headset.
- Permanent focus loss (incoming phone call) → end the call cleanly with the reason "interrupted".
- On call end: `clearCommunicationDevice()`, `MODE_NORMAL`, abandon focus.

### 6.4 v1.1 (not in v1)
Self-managed `ConnectionService` (Telecom) for full call-style integration (car/BT hands-free
buttons, coexistence with cellular calls).

---

## 7. Repository layout
```
lexi/
├─ CLAUDE.md                 # guidance for Claude CLI sessions
├─ docs/SPEC.md  docs/PLAN.md
├─ Cargo.toml                # workspace: core, minter, devtools/desk
├─ core/                     # lexi-core (cdylib for Android, rlib for desktop harness)
├─ devtools/desk/            # desktop harness: same rtc/openai/codec code + cpal audio
├─ minter/                   # lexi-minter service + Dockerfile
├─ android/                  # Gradle project, app module (Kotlin shell), jniLibs output
├─ deploy/k8s/               # kustomize base + overlays
└─ .github/workflows/        # CI: rust checks, android build, minter image, manifests
```

## 8. Build & tooling
- No Android Studio needed. Required: JDK 17+, Android cmdline-tools → `platform-tools`,
  `platforms;android-<target>`, `build-tools;<latest>`, `ndk;<latest LTS>`; Rust stable,
  `rustup target add aarch64-linux-android`, `cargo install cargo-ndk`.
- Android build: Gradle task `buildRustArm64` runs
  `cargo ndk -t arm64-v8a --platform 31 -o app/src/main/jniLibs build -p lexi-core --release`
  before `preBuild`.
- Release profile for core: `opt-level="z"`, `lto="fat"`, `codegen-units=1`, `strip=true`,
  `panic="abort"`. R8 enabled for Kotlin. Output a signed APK (`apksigner`) for sideloading.
- Lints: `cargo clippy -D warnings`, `cargo fmt --check`, `cargo deny` (licences + advisories).

## 9. Testing
- Rust unit tests: jitter buffer, Opus framing/timestamps, SDP munging, event parsing,
  state machine, minter handlers (mock OpenAI with `wiremock`).
- **Desktop harness** (`devtools/desk`): CLI that mints a token (or uses a dev key from env),
  connects to OpenAI with the **same** rtc/openai/codec modules, and uses `cpal` for audio.
  This is the main way to test the WebRTC path without a phone.
- Android instrumented smoke test: launch, grant permission, start/stop call against a
  minter in `off` attestation mode.
- Manual device matrix: one Pixel, one Samsung, one Xiaomi; speaker/earpiece/BT; lock
  screen for 10 min; Wi-Fi→LTE handover.

## 10. Acceptance criteria (v1)
1. Fresh install → tap Call → hear Lexi within 2.5 s on Wi-Fi.
2. Barge-in: speaking while Lexi talks stops her playback within 300 ms.
3. Lock the phone for 10 minutes mid-call → the conversation continues; the notification Hang up works.
4. Switch between speaker, earpiece and BT earbuds mid-call without dropping the call.
5. Toggle Wi-Fi off mid-call → reconnects within 5 s.
6. Visuals at ≥ 55 fps on a mid-range device; rendering stops when the screen is off.
7. Release APK size < 6 MB.
8. Minter: `kubectl apply -k deploy/k8s/overlays/dev` works on a kind cluster; p99 latency < 400 ms
   (dominated by upstream); no secrets in the repo; image runs as non-root.
