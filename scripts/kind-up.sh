#!/usr/bin/env bash
# Local dev: kind cluster + dev overlay + port-forward on localhost:8080.
#   OPENAI_API_KEY=sk-... scripts/kind-up.sh
# Requires docker, kind, kubectl. Attestation is `off` in the dev overlay; there is no ingress.
set -euo pipefail
cd "$(dirname "$0")/.."
: "${OPENAI_API_KEY:?set OPENAI_API_KEY}"
CLUSTER="${KIND_CLUSTER:-lexi}"

kind get clusters | grep -qx "$CLUSTER" || kind create cluster --name "$CLUSTER"
docker build -f server/Dockerfile -t lexi:dev .
kind load docker-image lexi:dev --name "$CLUSTER"

kubectl create namespace lexi --dry-run=client -o yaml | kubectl apply -f -
printf %s "$OPENAI_API_KEY" | kubectl -n lexi create secret generic openai-api-key \
  --from-file=OPENAI_API_KEY=/dev/stdin --dry-run=client -o yaml | kubectl apply -f -
kubectl apply -k deploy/k8s/overlays/dev
kubectl -n lexi rollout restart deploy/lexi >/dev/null
kubectl -n lexi rollout status deploy/lexi --timeout=180s

echo "port-forward on http://localhost:8080 (Ctrl-C to stop)"
exec kubectl -n lexi port-forward svc/lexi 8080:80
