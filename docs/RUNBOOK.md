# Lexi runbook (operating the server, keys, and releases)

Everything runs on the remote cluster (`ssh deploy.example.com`, namespace `lexi`, host `https://lexi.example.com`). Secrets exist
only on the cluster and in `~/.config/lexi` (release key); none is in the repo.

## Where the secrets are

| What | Where | Used for |
|---|---|---|
| OpenAI API key | Secret `openai-api-key` (`OPENAI_API_KEY`) | OpenAI tokens, OpenAI web search |
| Google API key | Secret `gemini-api-key` (`GEMINI_API_KEY`), optional | Gemini Live tokens (needs a funded/prepaid project) |
| Attestation keys | Secret `lexi-attest-key` (`LEXI_ATTEST_SHARED_KEY`, comma-separated) | which APK builds may call the server |
| Release signing key | `~/.config/lexi/release.jks` + `release.pass` (outside the repo) | signing the sideloaded APK |

The providers are independent: the server answers 503 for a provider whose key is missing and keeps serving the other.

## Deploy the server

```
set -a; . ./.envrc; set +a        # OPENAI_API_KEY, GOOGLE_API_KEY (the funded Google key)
scripts/deploy.sh             # git archive HEAD -> image on remote -> containerd -> overlay remote -> rollout
```
`.envrc` is only read the first time (OpenAI) and whenever `GOOGLE_API_KEY` is set (the Secret is refreshed).
The public host only routes `/v1/call-token` and `/v1/search`; check with a token request (see `deploy/k8s/README.md`).

## Attestation keys (who may use the server)

The app signs every token request with a key baked into the APK at build time. The server accepts every key listed in
`LEXI_ATTEST_SHARED_KEY` (comma-separated). One key per friend lets you revoke one person without touching the others;
two keys at once make a rotation painless.

```
# list how many keys are accepted (never print them)
ssh deploy.example.com "kubectl -n lexi get secret lexi-attest-key -o jsonpath='{.data.LEXI_ATTEST_SHARED_KEY}' | base64 -d | tr , '\n' | wc -l"

# add a friend: make a key, append it, restart the server
NEW=$(openssl rand -hex 32)
ssh deploy.example.com "OLD=\$(kubectl -n lexi get secret lexi-attest-key -o jsonpath='{.data.LEXI_ATTEST_SHARED_KEY}' | base64 -d); \
  printf %s \"\$OLD,$NEW\" | kubectl -n lexi create secret generic lexi-attest-key --from-file=LEXI_ATTEST_SHARED_KEY=/dev/stdin \
  --dry-run=client -o yaml | kubectl apply -f - && kubectl -n lexi rollout restart deploy/lexi"
# build that friend's APK (their key is baked in); keep a note of which key belongs to whom (name -> key) somewhere safe
LEXI_ATTEST_KEY=$NEW scripts/release-android.sh

# revoke: write the Secret again without that key and restart (the friend's APK then gets 401 at once)
```
Rotation of everything: add a new key, rebuild and hand out new APKs, then remove the old key from the Secret.
Timestamps must be within 5 minutes (phone clocks on automatic time are fine).

## Rotating the provider keys

```
printf %s "$NEW_OPENAI_KEY" | ssh deploy.example.com "kubectl -n lexi create secret generic openai-api-key --from-file=OPENAI_API_KEY=/dev/stdin --dry-run=client -o yaml | kubectl apply -f -"
ssh deploy.example.com "kubectl -n lexi rollout restart deploy/lexi"
```
(Google: same with `gemini-api-key` / `GEMINI_API_KEY`, or just run `scripts/deploy.sh` with `GOOGLE_API_KEY` set.)
Gemini needs a positive prepaid balance (aistudio.google.com/billing); an empty balance shows up as HTTP 402 /
WebSocket close 1011 "prepayment credits are depleted" and the app reports a service error for the Google provider only.

## Releases for sideloading

1. One time: `scripts/release-keystore.sh` (creates the key in `~/.config/lexi`). **Back up `release.jks` and
   `release.pass`.** If they are lost, every friend has to uninstall Lexi before an update installs (Android refuses a
   different signature), and their settings are gone.
2. `scripts/release-android.sh [--install]` builds the release APK on remote, aligns and signs it there (the key travels over
   ssh stdin and is deleted after) and writes `dist/lexi-<version>.apk`; it prints the signing certificate's SHA-256.
3. Send the APK to friends (they allow "install unknown apps" once). Bump `versionCode`/`versionName` in
   `android/app/build.gradle.kts` for every release so updates install over older ones.
4. A debug build and a release build have different signatures: uninstall one before installing the other.

## Sizes (N1)

`scripts/size-report.sh` builds the Android library on remote and prints the stripped size plus the biggest crates. Numbers
and what was tried are in `docs/PLAN.md` (Phase 7).

## Costs to keep an eye on

- OpenAI: tokens (see the on-screen total), plus $10 per 1000 web searches (+ about $0.004 of tokens each).
- Google: tokens, plus $14 per 1000 search queries (the first 5000 a month free).
- Both on-screen totals are estimates from the built-in price tables; compare with the providers' dashboards now and then.
