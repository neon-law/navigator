#!/usr/bin/env bash
# DocuSign demo rehearsal. --check-draft creates and reads an unsent envelope.
# --check-void sends one to embedded sandbox recipients (no email), voids it,
# and deletes a second draft.
# With no arguments, start the local signing ceremony.
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

# Local rehearsals use the demo account. CI exercises the HTTP adapter against Wiremock.
if [[ -n "${GITHUB_ACTIONS:-}" ]]; then
    echo "refusing: the ceremony is a local, credentialed rehearsal — never run it in CI" >&2
    exit 1
fi

fail() {
    echo "refusing: $1" >&2
    exit 1
}

if [[ ${1:-} == --check-draft || ${1:-} == --check-void ]]; then
    test_name=sandbox_reads_an_unregistered_draft_by_id
    [[ $1 == --check-void ]] && test_name=sandbox_voids_a_sent_envelope_and_deletes_a_draft
    shift
    if [[ $# -eq 2 && $1 == --project-id && -n $2 ]]; then
        project=$2
    elif [[ $# -gt 0 ]]; then
        fail "usage: $0 --check-draft|--check-void [--project-id <staging-project>]"
    else
        project=
    fi

    # Compile before loading any provider credentials into the test process.
    test_bin=$(cargo test -p server --test docusign_sandbox --no-run --message-format=json |
        jq -ner '[inputs | select(.target.name == "docusign_sandbox") | .executable // empty] | last')
    if [[ -n $project ]]; then
        read_secret() {
            gcloud secrets versions access latest --project="$project" --secret="DOCUSIGN_$1"
        }
        base=$(read_secret BASE_URL)
        oauth=$(read_secret OAUTH_BASE)
        [[ ${base%/} == https://demo.docusign.net/restapi ]] || fail "the selected account needs a demo REST base"
        case ${oauth%/} in
            https://account-d.docusign.com|account-d.docusign.com) ;;
            *) fail "the selected account needs the demo OAuth host" ;;
        esac
        export DOCUSIGN_SANDBOX_BASE_URL="$base" DOCUSIGN_SANDBOX_OAUTH_BASE="$oauth"
        for name in ACCOUNT_ID INTEGRATION_KEY USER_ID PRIVATE_KEY; do
            value=$(read_secret "$name")
            [[ -n $value ]] || fail "missing DOCUSIGN_$name"
            export "DOCUSIGN_SANDBOX_${name/PRIVATE_KEY/RSA_KEY}=$value"
        done
        unset value
    elif [[ -f .env.local ]]; then
        set -a
        # shellcheck disable=SC1091
        source .env.local
        set +a
    fi
    exec "$test_bin" --ignored --exact "$test_name" --nocapture
fi
[[ $# -eq 0 ]] || fail "usage: $0 [--check-draft|--check-void [--project-id <staging-project>]]"

# The local ceremony uses the worktree's environment and optional private overrides.
[[ -f .env || -f .env.local ]] || fail "configure .env or .env.local for the demo ceremony"
set -a
# shellcheck disable=SC1091
if [[ -f .env ]]; then source .env; fi
# shellcheck disable=SC1091
if [[ -f .env.local ]]; then source .env.local; fi
set +a

: "${NAVIGATOR_SIGNATURE_PROVIDER:?"set NAVIGATOR_SIGNATURE_PROVIDER=docusign in .env"}"
[[ "${NAVIGATOR_SIGNATURE_PROVIDER}" == "docusign" ]] ||
    fail "NAVIGATOR_SIGNATURE_PROVIDER is '${NAVIGATOR_SIGNATURE_PROVIDER}'; the ceremony rehearses the REAL provider — set it to docusign"
: "${DOCUSIGN_BASE_URL:?"fill the DOCUSIGN_* block of .env (see docs/docusign-esignature.md)"}"
: "${DOCUSIGN_ACCOUNT_ID:?"fill the DOCUSIGN_* block of .env (see docs/docusign-esignature.md)"}"
: "${DOCUSIGN_HMAC_KEY:?"set DOCUSIGN_HMAC_KEY — the receiver must verify the completion webhook fail-closed"}"

case "${DOCUSIGN_BASE_URL}" in
    https://demo.docusign.net/restapi|https://demo.docusign.net/restapi/) : ;;
    *) fail "DOCUSIGN_BASE_URL must name the demo host (…demo.docusign.net…); the ceremony never touches live envelopes" ;;
esac

echo "── DocuSign completion ceremony ────────────────────────────────────────"
echo "provider:  docusign (demo: ${DOCUSIGN_BASE_URL})"
echo "Configure the demo account-wide Connect receiver for the local Firm (HMAC-verified)."
echo
echo "Booting the dev server. Keep this terminal open; Ctrl-C ends the ceremony."
echo
if [[ -n "${CONNECT_WEBHOOK_URL:-}" ]]; then
    echo "Point DocuSign Connect (demo console → Connect) at the configured public webhook URL"
    echo "so completion can reach this machine; then send the dummy retainer from the walk."
else
    echo "DocuSign demo cannot call localhost: expose this server through a tunnel and"
    echo "re-run with CONNECT_WEBHOOK_URL=<public-url> to select the public receiver."
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
