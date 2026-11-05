#!/usr/bin/env bash
# DocuSign completion ceremony — the one credentialed piece, and it never
# runs in CI.
#
# What this script does: rehearse the REAL send against DocuSign demo on
# your machine. It checks the credentials, boots the `neon` dev server with
# `NAVIGATOR_SIGNATURE_PROVIDER=docusign`, and points you at the retainer
# walk to send the dummy retainer. You sign in DocuSign's demo signing
# page; DocuSign Connect delivers the completion webhook back to this
# server's receiver, which verifies the HMAC fail-closed and files the
# documents.
#
# What it deliberately does not do: click anything inside DocuSign. The
# signing step is yours — that is the point of a ceremony.
#
# Prerequisites: the `DOCUSIGN_*` block of `.env` filled (demo base URL;
# see docs/docusign-esignature.md), `DOCUSIGN_HMAC_KEY` set so the receiver
# verifies, and a tunnel (or port forward) if DocuSign demo must reach this
# machine over the internet — Connect cannot call `localhost`.
set -euo pipefail

# The ceremony is a local, credentialed rehearsal. CI proves the fake
# provider's loop instead (the dedicated "signature loop" job) and must
# never see credentials.
if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "refusing: the ceremony is a local, credentialed rehearsal — never run it in CI" >&2
    exit 1
fi

fail() {
    echo "refusing: $1" >&2
    exit 1
}

# Load .env the way the server does (dotenvy reads .env, then .devx/env);
# the checks below still demand the load-bearing ones explicitly.
if [[ ! -f .env ]]; then
    fail "no .env found — copy .env.example to .env and fill the DOCUSIGN_* block"
fi
# shellcheck disable=SC1091
set -a; source .env; set +a

: "${NAVIGATOR_SIGNATURE_PROVIDER:?"set NAVIGATOR_SIGNATURE_PROVIDER=docusign in .env"}"
[[ "${NAVIGATOR_SIGNATURE_PROVIDER}" == "docusign" ]] ||
    fail "NAVIGATOR_SIGNATURE_PROVIDER is '${NAVIGATOR_SIGNATURE_PROVIDER}'; the ceremony rehearses the REAL provider — set it to docusign"
: "${DOCUSIGN_BASE_URL:?"fill the DOCUSIGN_* block of .env (see docs/docusign-esignature.md)"}"
: "${DOCUSIGN_ACCOUNT_ID:?"fill the DOCUSIGN_* block of .env (see docs/docusign-esignature.md)"}"
: "${DOCUSIGN_HMAC_KEY:?"set DOCUSIGN_HMAC_KEY — the receiver must verify the completion webhook fail-closed"}"

case "${DOCUSIGN_BASE_URL}" in
    *demo*) : ;;
    *) fail "DOCUSIGN_BASE_URL must name the demo host (…demo.docusign.net…); the ceremony never touches live envelopes" ;;
esac

echo "── DocuSign completion ceremony ────────────────────────────────────────"
echo "provider:  docusign (demo: ${DOCUSIGN_BASE_URL})"
echo "receiver:  POST /webhook/esignature/${DOCUSIGN_WEBHOOK_SECRET:-local} (HMAC-verified)"
echo
echo "Booting the dev server. Keep this terminal open; Ctrl-C ends the ceremony."
echo
if [[ -n "${CONNECT_WEBHOOK_URL:-}" ]]; then
    echo "Point DocuSign Connect (demo console → Connect) at: ${CONNECT_WEBHOOK_URL}"
    echo "so completion can reach this machine; then send the dummy retainer from the walk."
else
    echo "DocuSign demo cannot call localhost: expose this server through a tunnel and"
    echo "re-run with CONNECT_WEBHOOK_URL=<public-url> so the receiver URL can be printed."
    echo "Without it, complete the walk and read the filed result from the provider directly."
fi
echo
echo "Opening the retainer walk once the server answers…"

cargo run -q -p neon &
SERVER_PID=$!
trap 'kill "${SERVER_PID}" 2>/dev/null || true' EXIT

# Wait for the server to answer before pointing a human at it.
for _ in $(seq 1 120); do
    if curl -fsS "http://127.0.0.1:${PORT:-3001}/health" >/dev/null 2>&1; then
        break
    fi
    if ! kill -0 "${SERVER_PID}" 2>/dev/null; then
        fail "the dev server exited during boot — read the log above"
    fi
    sleep 1
done

echo
echo "Server is up: http://127.0.0.1:${PORT:-3001}"
echo "Send the dummy retainer from the walk, sign in DocuSign demo, and watch this"
echo "log: the receiver's HMAC verification and the filed documents print here."
wait "${SERVER_PID}"
