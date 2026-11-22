# Sourced (not run) by the scripts in this directory. Loads .envrc when present, derives the service host and URL
# from LEXI_DEPLOY_HOST, and offers the shared checks. See .envrc.example.
LEXI_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [ -f "$LEXI_ROOT/.envrc" ]; then set -a; . "$LEXI_ROOT/.envrc"; set +a; fi

# Scripts that talk to the Kubernetes/build host call this first; it sets HOST and the service host/URL.
lexi_need_host() {
  if [ -z "${LEXI_DEPLOY_HOST:-}" ]; then
    echo "LEXI_DEPLOY_HOST is not set: copy .envrc.example to .envrc and fill it in (the ssh host with kubectl and docker)." >&2
    exit 1
  fi
  HOST="$LEXI_DEPLOY_HOST"
  LEXI_SERVICE_HOST="${LEXI_SERVICE_HOST:-lexi.$LEXI_DEPLOY_HOST}"
  LEXI_SERVICE_URL="${LEXI_SERVICE_URL:-https://$LEXI_SERVICE_HOST}"
  export HOST LEXI_SERVICE_HOST LEXI_SERVICE_URL
}

# Chat needs a provider key on the server; without any the deployment still works but nobody can talk.
lexi_warn_no_keys() {
  if [ -z "${OPENAI_API_KEY:-}" ] && [ -z "${GOOGLE_API_KEY:-}" ]; then
    echo "WARNING: neither OPENAI_API_KEY nor GOOGLE_API_KEY is set (.envrc). Chat will not be possible until the" >&2
    echo "         server has at least one provider key. Continuing anyway (set a key and run scripts/deploy.sh again)." >&2
  fi
}
