#!/usr/bin/env bash
# Build the service image on the Kubernetes node and deploy the `cluster` overlay (namespace `lexi`).
# Everything goes through ssh to $LEXI_DEPLOY_HOST (the Kubernetes API need not be reachable from here).
#
#   scripts/deploy.sh        # settings come from .envrc (see .envrc.example)
#
# OPENAI_API_KEY and GOOGLE_API_KEY, when set, create or refresh the Secrets openai-api-key and google-api-key;
# when unset the provider is simply not configured (the server rejects it, the app says so). With both unset the
# deployment still goes ahead and only warns that chat is not possible.
#
# Builds from the committed tree (git archive HEAD) so the image is reproducible from a SHA;
# unrelated uncommitted work elsewhere in the repo (e.g. core/) is ignored.
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/env.sh
lexi_need_host
lexi_warn_no_keys
NS=lexi

if [ -n "$(git status --porcelain -- server deploy .dockerignore)" ]; then
  echo "uncommitted changes in server/ or deploy/; commit first so the image maps to a SHA" >&2; exit 1
fi
TAG="$(git rev-parse --short=12 HEAD)"
REMOTE_DIR="/tmp/lexi-build-$TAG"

echo ">> shipping source @ $TAG"
git archive HEAD | ssh "$HOST" "rm -rf $REMOTE_DIR && mkdir -p $REMOTE_DIR && tar -C $REMOTE_DIR -xf -"

echo ">> building image lexi:$TAG (containerd store, visible to kubelet)"
{
  cat scripts/measure-lib.sh
  cat <<REMOTE
cd $REMOTE_DIR
measure_run "service image build" bash -c 'docker build -f server/Dockerfile -t lexi:$TAG . > /tmp/lexi-image-build.log 2>&1'
rc=\$?
tail -n 5 /tmp/lexi-image-build.log
exit \$rc
REMOTE
} | ssh "$HOST" bash -s

# The image is imported on one node, so the Deployment must run there: with a single-node cluster that is automatic.
NODE="${LEXI_NODE:-}"
if [ -z "$NODE" ]; then
  NODES="$(ssh "$HOST" "kubectl get nodes -o jsonpath='{.items[*].metadata.name}'")"
  case "$NODES" in *" "*|"") echo "the cluster has several nodes (or none): set LEXI_NODE (.envrc) to the node that builds and runs the image" >&2; exit 1;; esac
  NODE="$NODES"
fi
echo ">> importing image into the kubelet's containerd namespace (k8s.io)"
# docker's image store (containerd namespace "moby") is not what the kubelet reads, and our ssh user
# cannot talk to containerd directly. A short-lived Job in $NS mounts the containerd socket and the
# image tarball, then runs `ctr -n k8s.io images import`.
ssh "$HOST" "kubectl create namespace $NS --dry-run=client -o yaml | kubectl apply -f - >/dev/null; docker save lexi:$TAG -o $REMOTE_DIR/image.tar"
ssh "$HOST" "kubectl -n $NS delete job image-import --ignore-not-found >/dev/null && kubectl -n $NS apply -f -" <<JOB
apiVersion: batch/v1
kind: Job
metadata: { name: image-import }
spec:
  backoffLimit: 0
  ttlSecondsAfterFinished: 120
  template:
    spec:
      restartPolicy: Never
      nodeName: $NODE
      automountServiceAccountToken: false
      containers:
        - name: import
          image: ubuntu:22.04   # same glibc as the node, which the host ctr binary needs
          command: ["/host/ctr", "--address", "/run/containerd/containerd.sock", "-n", "k8s.io", "images", "import", "/import/image.tar"]
          volumeMounts:
            - { name: sock, mountPath: /run/containerd/containerd.sock }
            - { name: ctr, mountPath: /host/ctr, readOnly: true }
            - { name: tar, mountPath: /import, readOnly: true }
      volumes:
        - { name: sock, hostPath: { path: /run/containerd/containerd.sock, type: Socket } }
        - { name: ctr, hostPath: { path: /usr/bin/ctr, type: File } }
        - { name: tar, hostPath: { path: $REMOTE_DIR, type: Directory } }
JOB
ssh "$HOST" "kubectl -n $NS wait --for=condition=complete job/image-import --timeout=180s && kubectl -n $NS logs job/image-import | tail -n 3; kubectl -n $NS delete job image-import >/dev/null; rm -f $REMOTE_DIR/image.tar"

echo ">> namespace + secrets"
ssh "$HOST" "kubectl create namespace $NS --dry-run=client -o yaml | kubectl apply -f - >/dev/null"
# Provider keys, via stdin: a key never appears in a process list or shell history on either side. A key that is
# unset leaves any Secret already on the cluster untouched.
put_secret() { # name, env var name, value
  if [ -n "$3" ]; then
    printf %s "$3" | ssh "$HOST" "kubectl -n $NS create secret generic $1 --from-file=$2=/dev/stdin --dry-run=client -o yaml | kubectl apply -f -" >/dev/null
    echo "   $1: set from $2"
  elif ssh "$HOST" "kubectl -n $NS get secret $1 >/dev/null 2>&1"; then
    echo "   $1: $2 is unset here, keeping the Secret already on the cluster"
  else
    echo "   $1: not configured ($2 is unset): that provider will be rejected by the server"
  fi
}
put_secret openai-api-key OPENAI_API_KEY "${OPENAI_API_KEY:-}"
put_secret google-api-key GOOGLE_API_KEY "${GOOGLE_API_KEY:-}"
if ! ssh "$HOST" "kubectl -n $NS get secret lexi-attest-key >/dev/null 2>&1"; then
  ssh "$HOST" "openssl rand -hex 32 | tr -d '\n' | kubectl -n $NS create secret generic lexi-attest-key --from-file=LEXI_ATTEST_SHARED_KEY=/dev/stdin" >/dev/null
fi

# Optional custom prompt: LEXI_PROMPT_FILE replaces deploy/k8s/base/instructions.txt in the shipped copy (the repo file
# is not touched), so the ConfigMap, and with it the pods, change on the next rollout. See README "Changing the prompt".
if [ -n "${LEXI_PROMPT_FILE:-}" ]; then
  [ -f "$LEXI_PROMPT_FILE" ] || { echo "LEXI_PROMPT_FILE ($LEXI_PROMPT_FILE) does not exist" >&2; exit 1; }
  echo ">> using the custom prompt $LEXI_PROMPT_FILE"
  ssh "$HOST" "cat > $REMOTE_DIR/deploy/k8s/base/instructions.txt" < "$LEXI_PROMPT_FILE"
fi
# Optional NetworkPolicy (deploy/k8s/optional/networkpolicy.yaml): off by default because it names cluster-specific things.
case "${LEXI_NETWORK_POLICY:-off}" in
  on|1|true|yes) EXTRA="echo ---; cat ../../optional/networkpolicy.yaml"; CLEAN=":"; echo ">> NetworkPolicy: on (ingress namespace ${LEXI_INGRESS_NAMESPACE:-projectcontour})";;
  *) EXTRA=":"; CLEAN="kubectl -n $NS delete networkpolicy lexi --ignore-not-found"; echo ">> NetworkPolicy: off (set LEXI_NETWORK_POLICY=on to apply it)";;
esac
echo ">> applying overlay cluster (host $LEXI_SERVICE_HOST)"
ssh "$HOST" "cd $REMOTE_DIR/deploy/k8s/overlays/cluster && { kubectl kustomize .; $EXTRA; } | sed -e 's#image: lexi:local#image: lexi:$TAG#' -e 's#lexi\.example\.com#$LEXI_SERVICE_HOST#g' -e 's#cluster-issuer: letsencrypt#cluster-issuer: ${LEXI_CERT_ISSUER:-letsencrypt}#' -e 's#metadata.name: projectcontour#metadata.name: ${LEXI_INGRESS_NAMESPACE:-projectcontour}#' | kubectl apply -f - && $CLEAN && kubectl -n $NS rollout status deploy/lexi --timeout=180s"
echo ">> done: $LEXI_SERVICE_URL (cert-manager may need a minute for the certificate)"
