# Lexi runbook (operating the server, keys, and releases)

Everything runs on your cluster, reached through the deploy host (`ssh "$LEXI_DEPLOY_HOST"`, namespace `lexi`, public host
`$LEXI_SERVICE_HOST`; all three come from `.envrc`, see `.envrc.example`). Secrets exist only on the cluster, in your `.envrc`
and in `~/.config/lexi` (release key); none is in the repo. The commands below assume `. ./.envrc` has been run.

## Where the secrets are

| What | Where | Used for |
|---|---|---|
| OpenAI API key | Secret `openai-api-key` (`OPENAI_API_KEY`) | OpenAI tokens, OpenAI web search |
| Google API key | Secret `google-api-key` (`GOOGLE_API_KEY`), optional | Gemini Live tokens (needs a funded/prepaid project) |
| Attestation keys | Secret `lexi-attest-key` (`LEXI_ATTEST_SHARED_KEY`, comma-separated) | which APK builds may call the server |
| Release signing key | `~/.config/lexi/release.jks` + `release.pass` (outside the repo) | signing the sideloaded APK |

The providers are independent: the service answers HTTP 501 ("provider not configured") for a provider whose key is missing, the app tells the user to pick the other one, and the service keeps serving the other provider. With no key at all it still starts and `/readyz` says "no provider configured".

## Deploy the server

```
scripts/deploy.sh      # reads .envrc itself; git archive HEAD -> image on the node -> containerd -> overlay cluster -> rollout
```
Each provider key that is set in `.envrc` creates or refreshes its Secret; a key that is unset leaves any Secret already on the
cluster alone (and with no Secret that provider is simply not configured). With both unset the script warns that chat is not
possible and continues. The hostname and issuer come from `.envrc` (`LEXI_SERVICE_HOST`, `LEXI_CERT_ISSUER`).
The public host only routes `/v1/call-token` and `/v1/search`; check with a token request (see `deploy/k8s/README.md`).

## Attestation keys (who may use the server)

The app signs every token request with a key baked into the APK at build time. The server accepts every key listed in
`LEXI_ATTEST_SHARED_KEY` (comma-separated). One key per friend lets you revoke one person without touching the others;
two keys at once make a rotation painless.

```
# list how many keys are accepted (never print them)
ssh "$LEXI_DEPLOY_HOST" "kubectl -n lexi get secret lexi-attest-key -o jsonpath='{.data.LEXI_ATTEST_SHARED_KEY}' | base64 -d | tr , '\n' | wc -l"

# add a friend: make a key, append it, restart the server
NEW=$(openssl rand -hex 32)
ssh "$LEXI_DEPLOY_HOST" "OLD=\$(kubectl -n lexi get secret lexi-attest-key -o jsonpath='{.data.LEXI_ATTEST_SHARED_KEY}' | base64 -d); \
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
printf %s "$NEW_OPENAI_KEY" | ssh "$LEXI_DEPLOY_HOST" "kubectl -n lexi create secret generic openai-api-key --from-file=OPENAI_API_KEY=/dev/stdin --dry-run=client -o yaml | kubectl apply -f -"
ssh "$LEXI_DEPLOY_HOST" "kubectl -n lexi rollout restart deploy/lexi"
```
(Google: same with `google-api-key` / `GOOGLE_API_KEY`; or just put the new key in `.envrc` and run `scripts/deploy.sh`, which refreshes both Secrets.)
Gemini needs a positive prepaid balance (aistudio.google.com/billing); an empty balance shows up as HTTP 402 /
WebSocket close 1011 "prepayment credits are depleted" and the app reports a service error for the Google provider only.

## Releases for sideloading

1. One time: `scripts/release-keystore.sh` (creates the key in `~/.config/lexi`). **Back up `release.jks` and
   `release.pass`.** If they are lost, every friend has to uninstall Lexi before an update installs (Android refuses a
   different signature), and their settings are gone.
2. `scripts/release-android.sh [--install]` builds the release APK on the build host, aligns and signs it there (the key travels over
   ssh stdin and is deleted after) and writes `dist/lexi-<version>.apk`; it prints the signing certificate's SHA-256.
3. Send the APK to friends (they allow "install unknown apps" once). Bump `versionCode`/`versionName` in
   `android/app/build.gradle.kts` for every release so updates install over older ones.
4. A debug build and a release build have different signatures: uninstall one before installing the other.

## Sizes (N1)

`scripts/size-report.sh` builds the Android library on the build host and prints the stripped size plus the biggest crates. Numbers
and what was tried are in `docs/PLAN.md` (Phase 7).

## Costs to keep an eye on

- OpenAI: tokens (see the on-screen total), plus $10 per 1000 web searches (+ about $0.004 of tokens each).
- Google: tokens, plus $14 per 1000 search queries (the first 5000 a month free).
- Both on-screen totals are estimates from the built-in price tables; compare with the providers' dashboards now and then.

## Prompt pieces

The bot's prompt is `deploy/k8s/base/instructions.txt` (deployed; the default in the binary is
`server/src/instructions.default.txt`, kept identical by a test); `LEXI_PROMPT_FILE` in `.envrc` deploys another file
(`deploy/k8s/prompts/english.txt` is an English starting point). Smaller pieces live in code: the per-voice gender
paragraph (`Gender::instruction`, `server/src/config.rs`), the OpenAI `web_search` tool description and the search model's
instructions (`server/src/openai.rs`), the reminder returned with every search result (`how_to_answer` in
`core/src/call/mod.rs`) and the Gemini greeting trigger (`KICKOFF` in `core/src/gemini/mod.rs`).
