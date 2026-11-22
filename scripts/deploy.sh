#!/usr/bin/env bash
# Build the minter image on the remote node and deploy the `remote` overlay (namespace `lexi`).
# The Kubernetes API is not reachable from the internet, so everything goes through ssh.
#
#   OPENAI_API_KEY=sk-... scripts/deploy.sh     # key only needed the first time (creates the Secret)
#   GOOGLE_API_KEY=... also set (or GEMINI_API_KEY): creates/updates the Secret gemini-api-key (Gemini Live + search)
#
# Builds from the committed tree (git archive HEAD) so the image is reproducible from a SHA;
# unrelated uncommitted work elsewhere in the repo (e.g. core/) is ignored.
set -euo pipefail
HOST="${LEXI_DEPLOY_HOST:-deploy.example.com}"
NS=lexi
cd "$(dirname "$0")/.."

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
measure_run "minter image build" bash -c 'docker build -f server/Dockerfile -t lexi:$TAG . > /tmp/lexi-image-build.log 2>&1'
rc=\$?
tail -n 5 /tmp/lexi-image-build.log
exit \$rc
REMOTE
} | ssh "$HOST" bash -s

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
      nodeName: ${LEXI_NODE:-remote}
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
if ! ssh "$HOST" "kubectl -n $NS get secret openai-api-key >/dev/null 2>&1"; then
  : "${OPENAI_API_KEY:?set OPENAI_API_KEY for the first deploy (Secret openai-api-key does not exist)}"
  # via stdin: the key never appears in a process list or shell history on either side
  printf %s "$OPENAI_API_KEY" | ssh "$HOST" "kubectl -n $NS create secret generic openai-api-key --from-file=OPENAI_API_KEY=/dev/stdin" >/dev/null
fi
# Google Gemini Live (optional second provider): created or refreshed from GEMINI_API_KEY when it is set.
GOOGLE_KEY="${GOOGLE_API_KEY:-${GEMINI_API_KEY:-}}"   # GOOGLE_API_KEY (the funded project) wins
if [ -n "$GOOGLE_KEY" ]; then
  printf %s "$GOOGLE_KEY" | ssh "$HOST" "kubectl -n $NS create secret generic gemini-api-key --from-file=GEMINI_API_KEY=/dev/stdin --dry-run=client -o yaml | kubectl apply -f -" >/dev/null
fi
if ! ssh "$HOST" "kubectl -n $NS get secret lexi-attest-key >/dev/null 2>&1"; then
  ssh "$HOST" "openssl rand -hex 32 | tr -d '\n' | kubectl -n $NS create secret generic lexi-attest-key --from-file=LEXI_ATTEST_SHARED_KEY=/dev/stdin" >/dev/null
fi

echo ">> applying overlay remote"
ssh "$HOST" "cd $REMOTE_DIR/deploy/k8s/overlays/cluster && kubectl kustomize . | sed 's#image: lexi:remote#image: lexi:$TAG#' | kubectl apply -f - && kubectl -n $NS rollout status deploy/lexi --timeout=180s"
echo ">> done: https://lexi.example.com (cert-manager may need a minute for the certificate)"
