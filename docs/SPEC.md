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

---

## 2. Product behaviour

### 2.1 The coach
- Persona: friendly, concise vocabulary coach ("Lexi"). Helps the user learn and use words:
  definitions, nuance between synonyms, usage in sentences, etymology, quick spoken quizzes,
  pronunciation tips, word of the day.
- Keeps replies short (1–3 sentences) unless asked to elaborate — this also minimises cost
  (model speech is ~4× the price of user speech).
- Instructions, voice and turn detection are set **server-side by the minter** when the
  ephemeral key is created; the app only sends minimal `session.update` if needed.
- Language: English for v1; the instructions should be a config value in the minter so it can change without an app release.

### 2.2 Screen (single Activity, portrait + landscape)
- Full-screen `SurfaceView` with the visual.
- Overlay buttons (native Android `View`s, custom-drawn, large touch targets, content descriptions for accessibility):
  1. **Call / Hang up** (primary, centred bottom). Shows state: idle → connecting → live → ending.
  2. **Mute mic** (toggle).
  3. **Audio route** (cycles available outputs: Earpiece → Speaker → Bluetooth/Wired, showing only available ones; icon reflects current route).
  4. **Visual mode** (Fluid ↔ Fractal).
- Small status line (one text line) for errors such as "No network" or "Microphone permission needed".
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
  `mic_level` and `bot_level` (smoothed RMS, 0..1). The bot voice drives energy/colour;
  the user voice drives a secondary effect.
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

---

## 3. Non-functional requirements

| ID | Requirement |
|---|---|
| N1 | **Size**: Play download < **6 MB** (arm64-v8a). Rust `.so` < 4 MB stripped. No AndroidX Compose, no Kotlin coroutines library unless justified; only `androidx.core` allowed. |
| N2 | **minSdk 31** (Android 12; needed for `setCommunicationDevice`), **targetSdk** = current Google Play requirement **[VERIFY]**. ABIs: `arm64-v8a` (required), `armeabi-v7a` (optional build flag). |
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
startCall(minterUrl: String, installIdHash: String)   // runs async, reports via callback
endCall()
setMuted(muted: Boolean)
onAudioRouteChanged()                                 // restart AAudio streams if needed
onNetworkChanged()                                    // triggers reconnect if in call
NativeCallback.onState(state: Int, detail: String?)   // Idle/Connecting/Live/Reconnecting/Ended/Error
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
  - `response.done` → record `usage` for debug cost logging.
  - `error` → surface to UI and end the call if fatal.
- Audio: capture 48 kHz mono, `AAUDIO_INPUT_PRESET_VOICE_COMMUNICATION`,
  `AAUDIO_USAGE_VOICE_COMMUNICATION`, `AAUDIO_CONTENT_TYPE_SPEECH`, performance mode
  low-latency. If the device gives a different rate, resample (`rubato`) to 48 kHz.
- Opus encoder: VOIP application, 24–32 kbps, in-band FEC on, DTX on (silence costs nothing).

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
  Body: {}   (optional: {"locale":"en"})
  200 → {"token":"ek_...","expires_at":<unix>,"model":"gpt-realtime-2.1-mini"}
  401 attestation failed · 429 rate limited (Retry-After) · 502 upstream error · 503 not ready
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
  `LEXI_VOICE`, `LEXI_INSTRUCTIONS_FILE` (mounted ConfigMap), `LEXI_TOKEN_TTL_SECS`,
  `LEXI_RATE_PER_INSTALL` (default 30/hour), `LEXI_RATE_PER_IP` (default 120/hour),
  `LEXI_ATTEST_MODE` (`off` | `shared-key` | `play-integrity`).
- Rate limiting: in-memory per replica in v1 (`governor`); optional Redis backend behind a feature flag.
- Never log tokens, the API key, or the raw install ID. Redact the `Authorization` header in traces.
- Graceful shutdown on SIGTERM (drain ≤ 10 s). Upstream timeout 5 s, one retry on 5xx/connect errors.
- Image: static `x86_64-unknown-linux-musl` (and `aarch64` for ARM nodes) binary in
  `gcr.io/distroless/static:nonroot` or `scratch`. Target image < 15 MB.

### 5.4 Client attestation (abuse protection)
- `off`: development only.
- `shared-key`: the app sends an HMAC of a timestamp with a build-time key. Weak (extractable), MVP only.
- `play-integrity` (production): the app requests a Play Integrity token with a nonce
  derived from the request; the minter verifies it via Google's API and requires
  `PLAY_RECOGNIZED` + `MEETS_DEVICE_INTEGRITY` **[VERIFY current verdict names]**.

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
  `panic="abort"`. R8 enabled for Kotlin. Output an AAB.
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
7. AAB download size < 6 MB.
8. Minter: `kubectl apply -k deploy/k8s/overlays/dev` works on a kind cluster; p99 latency < 400 ms
   (dominated by upstream); no secrets in the repo; image runs as non-root.
