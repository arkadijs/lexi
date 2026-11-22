# desk — desktop harness for the Lexi WebRTC path

Uses the same `lexi_core::{openai, rtc, codec, jitter}` modules as the Android app; only audio I/O
(`cpal` here, AAudio on Android) differs.

```sh
# 1. a service (local, or the deployed one)
OPENAI_API_KEY=... cargo run -p lexi
# 2. talk to Lexi — use headphones (no echo cancellation on desktop)
cargo run -p desk -- --service http://localhost:8080
# dev only: skip the service, key from the environment (never a file or argument)
OPENAI_API_KEY=... cargo run -p desk -- --dev-key
```

Ctrl-C hangs up cleanly. Events and the per-response token `usage` are printed; tokens and keys never are.

## Testing without a microphone

```sh
# model speaks a reply to a text prompt; audio recorded to a WAV
desk --service URL --headless --prompt "Say hello." --exit-after-response --out-wav /tmp/lexi.wav
# use a WAV (48 kHz mono 16-bit) as the mic, e.g. to test barge-in 2.5 s into a long answer
desk --service URL --headless --trace --prompt "Speak a long passage..." --mic-wav hello.wav --mic-delay 2.5
# open real audio devices for 1 s, no network
desk --check-audio
```

## Verified

Model `gpt-realtime-2.1-mini` via `POST /v1/realtime/calls` (Realtime API; **not** GPT-Live).
str0m runs with the pure-Rust `rust-crypto` backend; DTLS against OpenAI works, no fallback to
`aws-lc-rs` was needed. See `core/src/openai/mod.rs` for the verified request/response details.
