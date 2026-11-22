# Deploying lexi

Kustomize layout: `base/` (Deployment, Service, Ingress, HPA, PDB, ConfigMaps), `optional/networkpolicy.yaml` (off by default),
`prompts/` (example bot prompts),
`overlays/prod` (shared-key attestation, trusts the ingress' `X-Forwarded-For`), `overlays/cluster` (prod for a cluster
without a registry: the image is built on the node, `imagePullPolicy: Never`), `components/servicemonitor` (optional,
needs the Prometheus Operator). The host name, the cert-manager issuer and the ingress controller's namespace are placeholders
in the manifests that `scripts/deploy.sh` fills in from `.envrc`.

## Secrets (never committed; referenced by the manifests)

`scripts/deploy.sh` creates them from `.envrc`. By hand:

```sh
kubectl -n lexi create secret generic openai-api-key   --from-literal=OPENAI_API_KEY=sk-...      # optional
kubectl -n lexi create secret generic google-api-key   --from-literal=GOOGLE_API_KEY=...         # optional
kubectl -n lexi create secret generic lexi-attest-key  --from-literal=LEXI_ATTEST_SHARED_KEY="$(openssl rand -hex 32)"
```
Both provider Secrets are optional: a provider without one is rejected by the service (HTTP 501) and the app says so.

## Optional pieces

- **NetworkPolicy** (`optional/networkpolicy.yaml`): off by default, enable with `LEXI_NETWORK_POLICY=on` in `.envrc`. It names
  the ingress controller's namespace (`LEXI_INGRESS_NAMESPACE`, default `projectcontour`), a `monitoring` namespace for
  metrics and CoreDNS in `kube-system`; adjust the file to your cluster first. See the top-level README.
- **Bot prompt**: `base/instructions.txt` is the deployed prompt (Russian-oriented); `LEXI_PROMPT_FILE` in `.envrc`
  deploys another one instead, e.g. `prompts/english.txt`.

## Deploying with scripts/deploy.sh

`scripts/deploy.sh` works through ssh to `$LEXI_DEPLOY_HOST`, so the Kubernetes API need not be reachable from your computer. It ships the committed tree
(`git archive HEAD`), builds the image with docker on the node, imports it into the kubelet's containerd
namespace with a one-off Job (docker's store is a different namespace and ssh users can't reach
containerd), creates the provider Secrets from `.envrc` (and the attestation Secret if missing), and applies `overlays/cluster` with your host name (plus the optional NetworkPolicy, and your own prompt if `LEXI_PROMPT_FILE` is set). Ingress (Contour, no ingress class,
`cert-manager.io/cluster-issuer: letsencrypt`) only routes `/v1/call-token` and `/v1/search`; health and metrics stay internal.

Calling the shared-key endpoint (key from the `lexi-attest-key` Secret):

```sh
TS=$(date +%s); SIG=$(printf %s "$TS" | openssl dgst -sha256 -hmac "$KEY" -hex | awk '{print $NF}')
curl -XPOST "$LEXI_SERVICE_URL/v1/call-token" -H "X-Lexi-Install: <64 hex>" -H "X-Lexi-Attest: $TS.$SIG" -d '{}'
```

Known gap: the cluster has no metrics-server, so the HPA reports `<unknown>` and will not scale
(the Deployment stays at its 2 replicas).
