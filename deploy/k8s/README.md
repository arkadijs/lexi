# Deploying lexi

Kustomize layout: `base/` (Deployment, Service, Ingress, HPA, PDB, NetworkPolicy, ConfigMaps),
`overlays/prod` (shared-key attestation, trusts the ingress' `X-Forwarded-For`), `overlays/cluster` (prod + host `lexi.example.com`,
locally built image), `components/servicemonitor` (optional, needs the Prometheus Operator).

## Secrets (never committed; referenced by the manifests)

```sh
kubectl -n lexi create secret generic openai-api-key   --from-literal=OPENAI_API_KEY=sk-...
kubectl -n lexi create secret generic lexi-attest-key  --from-literal=LEXI_ATTEST_SHARED_KEY="$(openssl rand -hex 32)"   # prod/remote
```

## deploy.example.com

The Kubernetes API is only reachable via ssh. `scripts/deploy.sh` ships the committed tree
(`git archive HEAD`), builds the image with docker on the node, imports it into the kubelet's containerd
namespace with a one-off Job (docker's store is a different namespace and ssh users can't reach
containerd), creates missing Secrets, and applies `overlays/cluster`. Ingress (Contour, no ingress class,
`cert-manager.io/cluster-issuer: letsencrypt`) only routes `/v1/call-token` and `/v1/search`; health and metrics stay internal.

Calling the shared-key endpoint (key from the `lexi-attest-key` Secret):

```sh
TS=$(date +%s); SIG=$(printf %s "$TS" | openssl dgst -sha256 -hmac "$KEY" -hex | awk '{print $NF}')
curl -XPOST https://lexi.example.com/v1/call-token -H "X-Lexi-Install: <64 hex>" -H "X-Lexi-Attest: $TS.$SIG" -d '{}'
```

Known gap: the cluster has no metrics-server, so the HPA reports `<unknown>` and will not scale
(the Deployment stays at its 2 replicas).
