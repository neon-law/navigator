#!/usr/bin/env bash
set -euo pipefail

# The first staging e2e dispatch (Actions run 43855678952) died at precheck:
# `$(api ...)` runs api in a subshell, so API_STATUS/API_HEADERS vanish before
# the parent shell reads them under `set -u`. The fix is api_capture, which
# runs api in the current shell and stages the body in a temp file. The third
# dispatch (run 43871368958) then died in seed's api_json — its own
# `$(api ...)` was still one level out from api's subshell, so api_json's
# status check read an unset global too. api_json now captures through
# api_capture. This test proves both shipped helpers propagate the globals a
# caller reads next.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
harness="${repo_root}/scripts/staging-jev-e2e.sh"

scratch="$(mktemp -d)"
trap 'rm -rf "${scratch}"' EXIT

# Lift api(), api_capture(), and api_json() out of the harness verbatim (each
# closes with a bare `}`), so the test exercises the shipped definitions, not
# a copy. die() is a one-liner with no bare `}`, so the test defines it.
awk '/^api\(\) \{/,/^\}/' "$harness" > "${scratch}/extracted.sh"
awk '/^api_capture\(\) \{/,/^\}/' "$harness" >> "${scratch}/extracted.sh"
awk '/^api_json\(\) \{/,/^\}/' "$harness" >> "${scratch}/extracted.sh"
grep -q '^api_capture() {' "${scratch}/extracted.sh"
grep -q '^api_json() {' "${scratch}/extracted.sh"

bash -euo pipefail -c "
  die() { echo '::error::'\"\$*\" >&2; exit 1; }
  source '${scratch}/extracted.sh'
  # Stub api exactly as the real one behaves: prints the body on stdout and
  # publishes the status and headers as globals in the shell that runs it.
  api() {
    printf '{\"ok\":true}\n'
    API_STATUS=200
    API_HEADERS='HTTP/2 200
x-navigator-next-cursor: [\"abc\"]'
  }
  # The caller pattern precheck(), run-assignment(), and cleanup() must use.
  api_capture POST /app/api/inbox/assign-matches '{\"apply\":false,\"after\":null}'
  body=\"\$(cat \"\$API_BODY_FILE\")\"; rm -f \"\$API_BODY_FILE\"
  [[ \"\$API_STATUS\" == 200 ]] || { echo 'API_STATUS did not propagate' >&2; exit 1; }
  grep -q 'x-navigator-next-cursor' <<<\"\$API_HEADERS\" || { echo 'API_HEADERS did not propagate' >&2; exit 1; }
  [[ \"\$body\" == *ok* ]] || { echo 'body capture failed' >&2; exit 1; }
  [[ ! -e \$API_BODY_FILE ]] || { echo 'body temp file was not removed' >&2; exit 1; }
  echo 'staging-jev-e2e: the api_capture pattern propagates API_STATUS, API_HEADERS, and the body.'

  # api_json's success path: seed() and friends capture it in a command
  # substitution, and its status check must see the globals in there.
  out=\"\$(api_json GET /app/api/entities)\"
  [[ \"\$out\" == *ok* ]] || { echo 'api_json success body failed' >&2; exit 1; }
  echo 'staging-jev-e2e: api_json returns the body through a command substitution.'

  # api_json's error path: a non-2xx must die with the status and body
  # excerpt — never with 'unbound variable' (run 43871368958).
  api() {
    printf '{\"error\":\"boom\"}\n'
    API_STATUS=404
    API_HEADERS='HTTP/2 404'
  }
  err_out=\"\$(api_json GET /app/api/entities 2>&1)\" && rc=0 || rc=\$?
  [[ \$rc -ne 0 ]] || { echo 'api_json accepted a 404' >&2; exit 1; }
  grep -q 'HTTP 404' <<<\"\$err_out\" || { echo \"api_json error lost the status: \$err_out\" >&2; exit 1; }
  grep -q 'boom' <<<\"\$err_out\" || { echo \"api_json error lost the body: \$err_out\" >&2; exit 1; }
  grep -q 'unbound variable' <<<\"\$err_out\" && { echo 'api_json error hit the subshell trap' >&2; exit 1; } || true
  echo 'staging-jev-e2e: api_json reports a non-2xx with the status and body, no unbound variable.'
"
