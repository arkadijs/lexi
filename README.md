# Lexi

<p align="center">
  <img src="docs/screenshots/openai-fluid.png" width="300" alt="A call with OpenAI and the fluid visual">
  &nbsp;&nbsp;
  <img src="docs/screenshots/gemini-julia.png" width="300" alt="A call with Google Gemini and the Julia fractal visual">
</p>
<p align="center"><em>A live call with OpenAI Realtime and the fluid visual (left), and with Google Gemini Live and the Julia fractal (right).</em></p>

Lexi is a voice chat bot for Android: tap call, talk, and it talks back in the language you speak, and it can search
the web when you ask about current events. The whole screen is a GPU visual (a Navier–Stokes fluid or a Julia fractal)
that reacts to the conversation. You pick the provider (**OpenAI Realtime** over WebRTC or **Google Gemini Live** over a
WebSocket), the voice and the model with the pills at the top; the call keeps running with the screen off, shows how long it has lasted and what it
costs so far, and survives network changes.

It is a hobby project that is sideloaded, not on a store, and built to be small: **the whole signed APK is about 5 MB**
(5.09 MB, of which the native library is 5.0 MB; no Compose, no coroutines, no third-party Android dependencies). It is a
Rust core (NDK) with a thin Kotlin shell, and a small Rust service on Kubernetes that holds the provider keys and hands
the app one short-lived token per call.

```
 phone (Kotlin shell + Rust core) ── token request ──▶ lexi service (Kubernetes) ── mints a short-lived token ──▶ OpenAI / Google
        │                                                         └── web search for the OpenAI bot
        └────────── audio (WebRTC to OpenAI, WebSocket to Google), directly ───────────────────────────────────▶ OpenAI / Google
```

- `core/` Rust library (NDK): WebRTC and WebSocket transports, audio (AAudio), Opus, GPU renderer, call supervisor, price tables.
- `android/` thin Kotlin shell (no dependencies): UI, foreground service, audio routing.
- `server/` Rust service `lexi`: issues one token per call, runs OpenAI web search. No provider key ever ships in the app.
- `devtools/desk/` desktop harness: the same core with real audio or WAV files (`cargo run -p desk -- --help`).
- `deploy/k8s/` kustomize manifests; `scripts/` build, deploy and release tooling; `docs/` the SPEC, PLAN, RUNBOOK and measurements.

## Prerequisites

**Provider keys** (kept on the service only; set at least one, either alone works, and a provider without a key is
rejected by the service and the app tells you so):

| Key | What you need |
|---|---|
| `OPENAI_API_KEY` | An OpenAI API key with billing enabled (platform.openai.com). Used for the Realtime models (`gpt-realtime-2.1-mini`, `gpt-realtime-2.1`) and for OpenAI's web search ($10 per 1000 searches plus a few thousand tokens each). |
| `GOOGLE_API_KEY` | A key for the Gemini API (Google AI Studio, or a Google Cloud project with the Generative Language API). A free-tier key can start a Gemini Live session but runs into tight quotas quickly. For real use the project needs **billing with prepaid credits**: new Gemini API accounts are on Google's *Prepay* plan (minimum top-up $5, set up under *Billing* in AI Studio), and the API only serves requests while the balance is positive. With an empty balance a call fails at once (HTTP 402, or the WebSocket closes with "prepayment credits are depleted") and the app shows "Service unavailable". Gemini 3 Live search is billed per query ($14 per 1000, the first 5000 a month free). |

Without any key the service still deploys and starts (the deploy script only prints a warning), but every call is rejected,
so chat is not possible until you add one.

**A Kubernetes cluster and a host to deploy from.** The scripts assume you have ssh access to a Linux machine (the
*deploy host*, `LEXI_DEPLOY_HOST`) on which `kubectl` already talks to your cluster and `docker` is installed; the cluster
API itself need not be reachable from your computer. What the cluster and your account need:

- **Permissions**, in the namespace `lexi` (the script creates it): create namespaces, and manage deployments, services,
  ingresses, HPAs, PodDisruptionBudgets, NetworkPolicies, ConfigMaps, Secrets and Jobs, and read pod logs. The image-import
  Job mounts the node's containerd socket with a `hostPath` volume, so pod-security admission must allow that in `lexi`.
- **An image path without a registry**: the image is built with docker on the node and imported into containerd with a
  one-off Job (`ctr` at `/usr/bin/ctr`, socket at `/run/containerd/containerd.sock`). That works for a single-node cluster,
  or for several nodes if you set `LEXI_NODE` and the Deployment runs there. If you have a registry, change the image
  settings in `deploy/k8s/overlays/cluster` and the import step in `scripts/deploy.sh`.
- **An ingress controller** (the manifests use no ingress class, so the cluster default) and **cert-manager** with a
  ClusterIssuer named `letsencrypt` (change with `LEXI_CERT_ISSUER`) for the TLS certificate.
- **DNS**: a record for `lexi.<deploy host>` pointing at the ingress (the service host is the deploy host name with `lexi.`
  in front, derived in `.envrc`).
- **Egress**: pods must reach `api.openai.com` and `generativelanguage.googleapis.com` on port 443. (A NetworkPolicy that
  restricts this is available but off by default, see below.)
- The cluster has no metrics-server in the original setup, so the HPA shows `<unknown>` and the Deployment simply stays at 2
  replicas.

**To build the app**: a machine with `git`, `ssh`, `openssl`, `python3` and `adb` (Android platform-tools), plus Rust
(`rustup` picks the toolchain from `rust-toolchain.toml`) if you want to run the tests or the desktop harness. The Android
toolchain (JDK, SDK, NDK, cargo-ndk) lives in a container image that is built on the first run on an x86_64 Linux build
host with docker (the deploy host works; first build takes several minutes and downloads the SDK). The phone must run
Android 12 or newer (arm64).

## Quick start

```sh
cp .envrc.example .envrc        # then fill it in: the provider keys and LEXI_DEPLOY_HOST (see the comments in the file)
scripts/deploy.sh               # builds the image on the deploy host, creates the Secrets, applies the manifests
scripts/android-remote.sh       # builds dist/app-debug.apk for the service in LEXI_SERVICE_URL (from .envrc)
adb install -r dist/app-debug.apk
```

`.envrc` is read by every script (and by direnv); it is git-ignored, so keys never reach the repository. The service host
and URL are derived from the deploy host (`lexi.<deploy host>`). For a build you hand to friends, with its own signing key
and a per-friend attestation key, see `docs/RUNBOOK.md` ("Releases for sideloading").

Try the pieces on your own machine first if you like:

```sh
cargo test --workspace                                    # host tests (add -j4 on a small machine)
OPENAI_API_KEY=... cargo run -p lexi                      # the service on :8080 (add GOOGLE_API_KEY=... for Google)
cargo run -p desk -- --service http://localhost:8080 [--provider google] [--voice kore] --greet
```

For a debug build that talks to a service on your computer, set `LEXI_SERVICE_URL=http://localhost:8080` and use
`adb reverse tcp:8080 tcp:8080` (cleartext is allowed to localhost in debug builds only).

## Changing the bot's prompt

The default prompt is written for the author: it greets in **Russian**, expects a Russian-speaking user and gives Russian
example answers. It is a plain text file, and the service reads it when it mints a token, so changing it needs no app
rebuild and takes effect on the next call:

- **Deployed service**: write your own prompt (start from `deploy/k8s/prompts/english.txt`, the same rules with an
  English greeting and examples), point `LEXI_PROMPT_FILE` at it in `.envrc`, and run `scripts/deploy.sh`. The script ships
  that file in place of `deploy/k8s/base/instructions.txt` (the repository copy is not modified) and the pods roll
  over to the new ConfigMap. To change the shared default instead, edit `deploy/k8s/base/instructions.txt` and keep
  `server/src/instructions.default.txt` identical (a test fails if they differ).
- **Local service** (`cargo run -p lexi`): set `LEXI_INSTRUCTIONS_FILE=/path/to/prompt.txt`. Without it the built-in default
  (`server/src/instructions.default.txt`) is used.

What the default prompt contains, so you know what to keep: the bot's name and the language rule (which language it
greets in and expects, and that it follows the user when they switch), a conversation style with hard length limits
(at most about 20 words per turn, no offers of help, no closing questions), the rule for when to use web search, and how
to react to "stop". Not in the file: the gender paragraph (so the bot says "рад"/"рада" correctly in languages that mark
it), which the service appends for each voice (`Gender::instruction` in `server/src/config.rs`, genders per voice in
`LEXI_VOICE_GENDERS` / `LEXI_GEMINI_VOICES`), and the small tool texts in the code (`docs/RUNBOOK.md`, "Prompt pieces").
Both providers receive the same prompt. Keep the instruction to greet exactly once, or the model tends to greet twice.

## The optional NetworkPolicy

`deploy/k8s/optional/networkpolicy.yaml` is **not deployed by default**, because it hard-codes cluster-specific names and
only matters if your CNI enforces NetworkPolicies. It restricts the pods to: ingress only from the ingress controller's
namespace (default `projectcontour`, set `LEXI_INGRESS_NAMESPACE`) and, for metrics, from a namespace called `monitoring`;
egress only to cluster DNS (CoreDNS in `kube-system`, label `k8s-app=kube-dns`) and to the internet on port 443,
never to private address ranges. Read it and adjust those names to your cluster, then set `LEXI_NETWORK_POLICY=on` in
`.envrc` and deploy. A wrong ingress namespace makes the service unreachable; switching it back to `off` and deploying
removes the policy again.

## Things to know before you run it

- **It costs money.** Every call is billed by the provider on your key; the app shows a running estimate from built-in
  price tables (rounded up to the cent; check them against the providers' pages and dashboards now and then).
- **Protecting the service.** The app signs token requests with a shared key baked into the APK (`LEXI_ATTEST_KEY`, kept on
  the cluster in a Secret). That is deliberately light: anyone who extracts it from an APK can use your service. Give each
  friend their own key so you can revoke one (`docs/RUNBOOK.md`); the service also rate-limits per install and per IP.
  Not meant for the public internet at scale.
- **Android only**, sideloaded, no Play Store (so no Play Integrity). Calls with the screen off, Bluetooth and wired
  headsets, and the earpiece, speaker and headset switching are supported; see `docs/FAILURE-MODES.md` for what was tested.
- **Size and power**: the native library is about 5 MB and the signed APK about 5 MB; the visuals are the main battery
  cost, a call with the fluid visual lasts roughly 9 hours on a 5000 mAh battery (`docs/POWER.md`).

## Documents

`docs/SPEC.md` (what and why), `docs/PLAN.md` (phases and status), `docs/RUNBOOK.md` (operating it: secrets, keys, releases),
`docs/FAILURE-MODES.md`, `docs/POWER.md`, `deploy/k8s/README.md`, `android/README.md`, `server/README.md`.

## License

BSD 2-Clause, see `LICENSE`. This is an independent project; it is not affiliated with or endorsed by OpenAI or Google.
OpenAI and Gemini are trademarks of their owners.
