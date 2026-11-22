# lexi

Holds the OpenAI API key and issues **one ephemeral client secret per call** (SPEC §5).

## Run locally

```sh
OPENAI_API_KEY=sk-... cargo run -p lexi
curl -XPOST localhost:8080/v1/call-token -H "X-Lexi-Install: $(printf 'a%.0s' $(seq 64))" -d '{}'
```

Port 8080: `POST /v1/call-token`, `/healthz`, `/readyz`. Port 9090: `/metrics`.

## Configuration (env)

| Variable | Default | Notes |
|---|---|---|
| `OPENAI_API_KEY` | – | Without it the service runs but `/readyz` and minting return 503. |
| `LEXI_MODEL` | `gpt-realtime-2.1-mini` | |
| `LEXI_VOICE` | `cedar` | Default voice when the app does not ask for one. |
| `LEXI_VOICES` | all ten built-in voices | Voices the app may request via `{"voice":"cedar"}` in the token request; anything else is a 400. |
| `LEXI_REASONING_EFFORT` | `low` | `session.reasoning.effort` (`minimal`/`low`/`medium`/`high`/`xhigh`), always sent explicitly; `default` = omit it and let the API choose (`low`). `minimal` measured ~0.2 s faster to first audio. Set in the k8s ConfigMap, so it changes without an app release. |
| `LEXI_VAD_SILENCE_MS` | `400` | server_vad `silence_duration_ms` (API default 500; 0 = default). Shorter = faster reply, but may jump in on short pauses. |
| `LEXI_INSTRUCTIONS_FILE` | built-in prompt (language behaviour only: greet in Russian, follow a language switch) | Mounted ConfigMap in k8s. |
| `LEXI_TOKEN_TTL_SECS` | `60` | Must be 10..=7200 (OpenAI limits). |
| `LEXI_RATE_PER_INSTALL` / `LEXI_RATE_PER_IP` | `30` / `120` per hour | In-memory, per replica. |
| `LEXI_ATTEST_MODE` | `off` | `off` \| `shared-key` (needs `LEXI_ATTEST_SHARED_KEY`: one key or several, comma-separated, one per friend; see docs/RUNBOOK.md). |
| `LEXI_TRUST_FORWARDED_FOR` | `false` | Use the rightmost `X-Forwarded-For` entry as client IP (only behind a trusted ingress). |
| `LEXI_BIND` / `LEXI_METRICS_BIND` | `0.0.0.0:8080` / `0.0.0.0:9090` | |
| `LEXI_UPSTREAM_TIMEOUT_MS` / `LEXI_REQUEST_TIMEOUT_MS` | `5000` / `12000` | |
| `LEXI_OPENAI_BASE_URL` | `https://api.openai.com` | Tests point this at wiremock. |

`shared-key` header: `X-Lexi-Attest: <unix-ts>.<hex(hmac_sha256(key, ts))>`, ±300 s skew.

## Verified against the OpenAI docs (2026-11-21)

Sources: the API reference for *Realtime → Client secrets → Create* and the *Realtime WebRTC* guide
(both on developers.openai.com).

- `POST /v1/realtime/client_secrets`, `Authorization: Bearer <API key>`, JSON body.
- Body: `expires_after {anchor: "created_at", seconds}` (default 600, **min 10, max 7200**) and
  `session {type: "realtime", model, instructions, audio.output.voice, audio.input.turn_detection}`.
  `gpt-realtime-2.1-mini` is listed in the `session.model` enum. Voice `marin` is a listed built-in.
  `turn_detection` accepts `{type: "server_vad", interrupt_response, ...}`.
- Response: `value` (the `ek_...` secret), `expires_at` (unix seconds), `session`. We use only the first two.
- `OpenAI-Safety-Identifier: <hashed id>` is set by the trusted backend (the guide's server example);
  we send the install hash. The reference page does not list it in its header section.
- WebRTC (used in Phase 3, not here): `POST /v1/realtime/calls` with `Authorization: Bearer <ek_...>`,
  `Content-Type: application/sdp`, body = offer, response = answer SDP; data channel label `oai-events`.

Not confirmed: the response schema was only seen as a summary (field names match the above, and the
real call returned them); the guide now also advertises a separate "GPT-Live" `/v1/live/sessions`
API, which we do not use — it is unclear whether it applies to Realtime sessions. Re-check in Phase 3.

## Tests

`cargo test -p lexi` — wiremock covers success, upstream 4xx/5xx/timeout/bad body, rate limits,
attestation and readiness. `tests/logs.rs` (separate binary on purpose) asserts that the API key, the
ephemeral token, the install hash and the attestation material never appear in logs.
