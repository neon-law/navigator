#!/usr/bin/env bash
set -euo pipefail

# The first staging e2e dispatch (Actions run 43855678952) died at precheck:
# `$(api ...)` runs api in a subshell, so API_STATUS/API_HEADERS vanish before
# the parent shell reads them under `set -u`. The fix is api_capture, which
# runs api in the current shell and stages the body in a temp file. This test
# proves that capture pattern propagates the globals a caller reads next.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
harness="${repo_root}/scripts/staging-jev-e2e.sh"

scratch="$(mktemp -d)"
trap 'rm -rf "${scratch}"' EXIT

# Lift api() and api_capture() out of the harness verbatim (each closes with a
# bare `}`), so the test exercises the shipped definitions, not a copy.
awk '/^api\(\) \{/,/^\}/' "$harness" > "${scratch}/extracted.sh"
awk '/^api_capture\(\) \{/,/^\}/' "$harness" >> "${scratch}/extracted.sh"
grep -q '^api_capture() {' "${scratch}/extracted.sh"

bash -euo pipefail -c "
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
"
