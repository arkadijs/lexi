# Lexi — Product & Technical Specification

> **Lexi**: a generic voice chat bot for Android. The user "calls" an AI
> that talks back, can search the web for current events, and answers in the language they speak; the single
> screen shows GPU-rendered fluid or fractal visuals that react to the conversation. Two providers: OpenAI
> Realtime (WebRTC) and Google Gemini Live (WebSocket).

Status: v1 spec. Items marked **[VERIFY]** depend on external APIs that change often — check
the current official docs before implementing them.

---

## 1. Goals and non-goals

### Goals
- G1. Voice conversation with an AI over **WebRTC** directly to the **OpenAI Realtime API** (`gpt-realtime-2.1-mini`
  "Mini" by default, `gpt-realtime-2.1` "Smart" selectable), or over a **WebSocket** to **Google Gemini Live**
  (`gemini-3.8-live`); the user picks the provider (§2.1).
- G2. Audio works on the **loudspeaker, earpiece and wired/Bluetooth earbuds**, user-switchable.
- G3. The call **keeps working when the phone is locked** or the app is in the background.
- G4. One screen: full-screen **GPU visuals** (Navier–Stokes fluid or fractal explorer) + a few custom buttons.
- G5. **Small and efficient**: Rust core via NDK, thin Kotlin shell, no heavy frameworks.
- G6. No provider API key ships in the app. A **Rust service on Kubernetes** (`lexi`) issues one short-lived
  ephemeral key per call and runs the web searches.

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

### 2.1 The bot
- **Prompt** (rewritten 2026-11-22; text in `server/src/instructions.default.txt`, mirrored in the
  `lexi-instructions` ConfigMap `deploy/k8s/base/instructions.txt`, a test keeps them identical, so it
  changes without an app release). Two parts:
  1. *Language:* the model greets the user with one short sentence in Russian before they say anything, expects a
     Russian-speaking user and answers in Russian by default, switches language when the user does (or asks it to) and
     stays in it until they switch again, and speaks naturally with native pronunciation.
  2. *Conversation style:* an equal conversation partner, not an assistant: relaxed, direct, with opinions. **Hard limits**
     (OpenAI's guide says "be concise" alone works worse than limits defined per situation): every turn at most 20
     words (usually 5-12); longer only when the user explicitly asks to elaborate, then at most four sentences; answer
     first, no recap, no closing remark. Never offers help or extras ("anything else?"); asks a question back only when
     it really needs the answer; a short acknowledgement from the user needs no reply. **When told to stop** (or
     interrupted) it answers with at most two words or nothing and stays silent until addressed, and does not resume.
     A few short example exchanges are included (the model follows examples closely; the prompt says to vary them).
  Why: the model kept talking and, after a barge-in, answered the user's "stop" with a new helpful monologue (the
  server starts a new response for every utterance). Measured on the live API with text prompts (seconds of bot speech,
  old prompt -> new prompt): small talk ~12 -> 2.7-4.2; a definition 18-20 -> 2.1-4.8; "stop" 1.8-4.7 -> 0.3-1.3; "ok,
  got it" 3.7-8.3 -> 1.5-3.6; "tell me about octopuses in detail" 39-62 -> 10-14. Against the deployed server: a stop
  request is answered in 0.7 s. Limits: replies to opinion questions still run 6-12 s, and barge-in with real audio was
  not part of this test.
- **Greeting trigger:** instructions alone do not make the model speak first. The core sends `response.create` once
  when the `oai-events` channel opens (once per call: a reconnect does not greet again). It costs a few audio tokens.
- **Not a coach:** the product is a general conversation partner. The bot is named **Lexi** and says so; it greets
  **exactly once** (a later "hello" from the user gets at most one word); the greeting includes its name. Verified
  2026-11-22: on 12 sampled greetings (2 models x 3 voices x 2) 11 had one response; one Mini/Marin run added a second,
  unprompted one; the user then reported the double greeting gone in real use.
- **Gender:** the server appends a per-voice gender line to the instructions (`LEXI_VOICE_GENDERS`: Cedar male; Marin
  and Alloy female; Gemini: Charon and Fenrir male, Kore and Leda female) so the bot says "рад" or "рада" about itself
  in Russian and the like in other gendered languages. Verified by speech-to-text on both OpenAI models and on Gemini.
- Instructions, voice, turn detection and reasoning effort are set **server-side by the server** when the ephemeral
  key is created; the app only chooses provider, voice and (OpenAI only) model (below).
- **Provider:** a pill to the left of the voice pill selects **OpenAI** (default) or **Google**. Sent as `provider` in
  the token request (§5.2), remembered, locked during a call together with the voice and model pills. The voice list
  follows the provider (OpenAI: Cedar, Marin, Alloy; Google: Charon, Fenrir, Kore, Leda, with a remembered choice per
  provider); with Google the model pill shows "Live" and is disabled (one Live model, `gemini-3.8-live`).
- **Google Gemini Live** (verified 2026-11-22 against the live API; docs at aistudio.google.com/docs/live-api). Live is
  **WebSocket only** (no WebRTC): the server mints a one-use ephemeral token with `POST /v1beta/auth_tokens`, whose
  `bidiGenerateContentSetup` field locks model, voice, instructions (with the gender line), Google's `googleSearch` tool
  and audio output (the docs' `liveConnectConstraints` is rejected by the API); the app connects to
  `wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContentConstrained?access_token=<token>`
  and sends an empty `setup`. Audio: 16 kHz 16-bit PCM from the mic (40 ms messages, `audio/pcm;rate=16000`), 24 kHz
  PCM back in bursts faster than real time, so the core queues it and paces 20 ms frames into the playout ring;
  `serverContent.interrupted` is barge-in; Google sends JSON in binary frames. Greeting: a text turn
  (`realtimeInput.text`) when the session opens. **Web search is Google's own `googleSearch` tool, run inside the Live
  session** (nothing for the app to answer; the model decides when to search and answered a news question 1.6 s after the
  question). `serverContent.groundingMetadata` reports the queries, which the core bills at $14 per 1000 on top of the
  tokens (the first 5000 a month are free, so the readout can only err high). Google search needs a funded project (on a
  free-tier key the session was closed with 1011 "exceeded your current quota"). `usageMetadata` arrives once per generation with the whole prompt
  again, so each is a delta; thinking tokens are billed as output. Measured: connect to live 1.4 s on the phone;
  Gemini 3.8 Live spends ~100-300 thinking tokens per turn (a `thinkingLevel` knob exists, `LEXI_GEMINI_THINKING`,
  untested). Sessions last about 15 minutes of audio; that limit is accepted (decided 2026-11-22): no session resumption, the call simply ends (shown as ended by the other side) and the user calls again. Gemini billing needs a prepaid balance
  (HTTP 402 / close 1011 "prepayment credits are depleted" otherwise).

- **Providers are compartmentalized.** Each provider has its own server module and Secret, its own core transport
  and its own search: OpenAI = `server/src/openai.rs` (token + `POST /v1/search`, the Responses `web_search` tool),
  `core/src/rtc` + `core/src/openai` (WebRTC, tool loop); Google = `server/src/gemini.rs` (token only),
  `core/src/gemini` (WebSocket; search runs inside the session). Neither depends on the other's key: without
  `GOOGLE_API_KEY` the Google provider answers HTTP 501 ("provider not configured", shown in the app as "This provider is not set up on the server") and OpenAI is unaffected, and the reverse. Only the call
  supervisor, the pricing table, the app UI and the shared prompt are common. (A Gemini `generateContent` search
  backend for the OpenAI bot measured 1.5 s against OpenAI's 5.7 s, but was not adopted: it would make the OpenAI bot
  depend on a Google key.)
- **Voice:** selectable from a popup at the top of the screen (§2.2) with three entries, **Cedar** (the default),
  **Marin** and **Alloy**, no section headers. The server accepts all ten built-in voices (`LEXI_VOICES`); only the app's menu is
  narrowed. The choice is sent as `voice` in the token request (§5.2), validated by the server, remembered across
  launches (a stored voice that is no longer offered falls back to Cedar) and **fixed for the whole call**: the API
  locks the voice once the model has spoken, so both selectors are disabled (dimmed, not clickable) while a call runs.
- **Model:** a second pill beside the voice pill (same width, symmetric around the centre) chooses **Mini**
  (`gpt-realtime-2.1-mini`, the default) or **Smart** (`gpt-realtime-2.1`). Sent as `model` in the token request, validated
  against the server's `LEXI_MODELS`, remembered, locked during a call. The prompt was checked against both models
  (2026-11-22, text prompts through the desk harness): replies are equally short on Smart, "stop" is answered in 0.8-1.7 s.
- **Name and web search:** the bot is called **Lexi** (the prompt says so and the greeting includes it). The session has
  one function tool, `web_search(query)`. Realtime has no hosted web search (tool types: `function`, `mcp`), so the app
  answers the call: `response.done` carries a `function_call` item, the core calls the server's `POST /v1/search`
  (attested, rate limited; the server runs the Responses API `web_search` tool with `gpt-4.1-mini` and returns two plain
  sentences), sends `conversation.item.create` (`function_call_output`) and `response.create`. The prompt tells the bot to
  say a short filler first ("Секунду, гляну"); a search takes about 3-6 s. Each search adds an estimated $0.015 to the
  call cost ($10 per 1000 calls plus a fixed 8 000 input tokens at gpt-4.1-mini rates). Off with `LEXI_WEB_SEARCH=false`.
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
  (the API default; `minimal` is ~0.2 s faster but was not adopted). Both are server settings, set in its ConfigMap and
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
   fetches an ephemeral key from the server → builds the WebRTC offer → POSTs it to OpenAI
   → applies the answer → audio flows.
3. The call continues with the screen off or locked. An ongoing notification shows
   "Talking with Lexi", with **Mute** and **Hang up** actions.
4. The call ends on: Hang up (UI or notification), remote close, fatal error, audio focus
   loss to a phone call (see §6.3), or the max-session guard (configurable, default 30 min).
5. Network change (Wi-Fi ↔ cellular) → automatic reconnect with a **new** ephemeral key
   (backoff 0.5 s/1 s/2 s; the first 3 attempts always, then keep retrying every 2 s until 20 s after the connection was
   lost, as long as the failure can pass: no network, service down; auth, clock and rate-limit errors stop at once). The
   window was added after the failure-mode tests (a 5 s Wi-Fi drop ended a Gemini call). The conversation context is lost
   on reconnect in v1.

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

  `gpt-realtime-2.1` (Smart), same source, verified 2026-11-22:

  | Modality | Input | Cached input | Output |
  |---|---|---|---|
  | Audio | $32.00 | $0.40 | $64.00 |
  | Text | $4.00 | $0.40 | $24.00 |
  | Image | $5.00 | $0.50 | not listed |

  `gemini-3.8-live` (Google, paid tier; from the pricing page via a summary, **not yet checked against a bill**):
  text $0.75 in / $4.50 out, audio $3.00 in / $12.00 out, image/video $1.00 in; no cached rate known, cached input is
  priced like fresh input (the readout can only err high); thinking tokens count as output text.

  The on-screen total is **rounded up to the next cent** ($0.0011 shows as $0.01).

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
  when a new call starts. It stays on screen for 10 s after the call ends and then disappears.
- **Unknown model:** the server returns the model it used. If it is not in the built-in table (for example a changed
  `LEXI_MODEL`), the readout is hidden rather than wrong.
- **Display:** US dollars, four decimals below $1 (`$0.0123`), two above. An estimate to be compared with the OpenAI
  usage dashboard; the table carries a `VERIFIED_ON` date and must be re-checked when prices or the model change.

---

## 3. Non-functional requirements

| ID | Requirement |
|---|---|
| N1 | **Size**: release APK < **6 MB** (arm64-v8a) and Rust `.so` < **6 MB** stripped (relaxed from 4 MB on 2026-11-22: measured 5.01 MB, the rest is dependencies we need). No AndroidX Compose, no Kotlin coroutines library unless justified; only `androidx.core` allowed. |
| N2 | **minSdk 31** (Android 12; needed for `setCommunicationDevice`), **targetSdk** 36 (the current Play requirement, verified 2026-11-21; not mandatory for a sideloaded app, but there is no reason to run in older compatibility modes). ABIs: `arm64-v8a` (required), `armeabi-v7a` (optional build flag). |
| N3 | **Latency**: tap-to-first-audio < 2.5 s on 4G (excluding model think time). Mouth-to-ear added by the client < 80 ms (20 ms frames + ≤ 60 ms playout buffer). |
| N4 | **CPU/battery**: during a call with the screen off, app CPU < 5 % of one big core on a mid-range device; no rendering, no busy loops. |
| N5 | **Robustness**: no crash on permission denial, airplane mode, BT headset connect/disconnect mid-call, or the server being unreachable. Rust panics must not cross FFI (`catch_unwind` at every JNI entry point); `panic = "abort"` only in release, after FFI guards. |
| N6 | **Security**: no OpenAI key in the APK or the repo. The ephemeral key is held only in memory and never logged. TLS everywhere (rustls). |
| N7 | **Privacy**: no audio stored on device or server. The server logs no PII; the install ID is hashed before being sent as the OpenAI safety identifier. |

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
│ openai/   token fetch (server), SDP exchange, event JSON     │
│ jitter/   playout buffer (60 ms target, PLC on loss)         │
│ state/    call state machine, shared atomics (levels, mute)  │
└──────────────┬──────────────────────────────────────────────┘
               │ HTTPS
   ┌───────────▼───────────┐        ┌─────────────────────────┐
   │ lexi (Rust,    │ HTTPS  │ OpenAI                  │
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
startCall(serverUrl: String, installIdHash: String, attestKey: String,
          voice: String, model: String, provider: String)   // runs async, reports via callback;
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

## 5. Token-minting service — `lexi`

### 5.1 Purpose
Holds the OpenAI API key. Issues **one ephemeral client secret per call**, with the session
configuration (model, instructions, voice, turn detection) fixed server-side.

### 5.2 API
```
POST /v1/call-token
  Headers: X-Lexi-Install: <sha256(install-id) hex>
           X-Lexi-Attest: <app attestation token>          (see 5.4)
  Body: {}   (optional: {"voice":"cedar","model":"gpt-realtime-2.1","provider":"openai","locale":"en"}; voice must be one of
             LEXI_VOICES (OpenAI) or LEXI_GEMINI_VOICES (provider "google"), model one of LEXI_MODELS and not
             allowed with "google"; "google" answers {"token":"auth_tokens/...","model":"gemini-3.8-live",
             "provider":"google"} and needs GOOGLE_API_KEY, else 501)
  200 → {"token":"ek_...","expires_at":<unix>,"model":"gpt-realtime-2.1-mini","provider":"openai"}
  400 invalid body / unknown voice / unknown model / unknown provider · 401 attestation failed · 429 rate limited (Retry-After) ·
  501 provider not configured (no key for the requested provider) · 502 upstream error · 412 clock skew
POST /v1/search  (same headers; separate rate limits LEXI_RATE_SEARCH_PER_INSTALL=60/h, _PER_IP=240/h)
  Body: {"query":"..."}  (1-300 chars)   200 → {"text":"two plain sentences"}   404 when LEXI_WEB_SEARCH=false
GET /healthz   (liveness, no dependencies)
GET /readyz    (ready once config is loaded; the body lists the configured providers, or says none is configured)
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
  `LEXI_ATTEST_MODE` (`off` | `shared-key`).
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
  server, rebuild the APK; old builds then get 401 until updated. If one friend's copy leaks or someone leaves,
  per-friend keys (several accepted keys on the server) are the next step.
- Play Integrity is not used: it only works for apps distributed through Google Play (the stub was deleted in Phase 7).

### 5.5 Kubernetes
- Manifests with **kustomize** in `deploy/k8s/` (`base/` + `overlays/prod`, plus `overlays/cluster` for the real cluster):
  Deployment (2+ replicas, `runAsNonRoot`, read-only root FS, all capabilities dropped,
  seccomp `RuntimeDefault`, requests 20m CPU / 32Mi, limits 200m / 64Mi), Service,
  Ingress (TLS via cert-manager annotation, host configurable), HPA (CPU 70 %),
  PodDisruptionBudget (minAvailable 1), ConfigMap (instructions + non-secret config),
  Secret reference (`openai-api-key`, created out-of-band or via External Secrets; **never committed**),
  an optional NetworkPolicy (`deploy/k8s/optional/`, off by default: ingress from the ingress controller only, egress DNS + 443;
  enabled with `LEXI_NETWORK_POLICY=on`), ServiceMonitor (optional).
- Probes: liveness `/healthz`, readiness `/readyz`, startup probe.
- No CI for now (the workflows were removed before publishing); the image is built on the node by `scripts/deploy.sh`.

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
  Default priority: Bluetooth > wired headset > **speaker** (the bot is used hands-free) >
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
├─ Cargo.toml                # workspace: core, server (crate `lexi`), devtools/desk
├─ core/                     # lexi-core (cdylib for Android, rlib for desktop harness)
├─ devtools/desk/            # desktop harness: same rtc/openai/codec code + cpal audio
├─ server/                   # lexi service + Dockerfile
├─ android/                  # Gradle project, app module (Kotlin shell), jniLibs output
├─ deploy/k8s/               # kustomize base + overlays
└─ .github/workflows/        # CI: rust checks, android build, server image, manifests
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
  state machine, server handlers (mock OpenAI with `wiremock`).
- **Desktop harness** (`devtools/desk`): CLI that mints a token (or uses a dev key from env),
  connects to OpenAI with the **same** rtc/openai/codec modules, and uses `cpal` for audio.
  This is the main way to test the WebRTC path without a phone.
- Android instrumented smoke test: launch, grant permission, start/stop call against a
  server in `off` attestation mode.
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
8. Server: `scripts/deploy.sh` rolls out to the cluster; p99 latency < 400 ms
   (dominated by upstream); no secrets in the repo; image runs as non-root.
