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
server_pid=""
cleanup() {
  if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || true; fi
  rm -rf "${scratch}"
}
trap cleanup EXIT

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

  # The fifth staging dispatch (Actions run 43885192582) got a 2xx whose body
  # was cut mid-string server-side; bare jq then failed with exit 5 and no
  # diagnosis. api_json must validate the body itself and die with the
  # method, path, status, content-length, and a visible excerpt.
  api() {
    printf '{\"data\":[{\"id\":\"per_'
    API_STATUS=200
    API_HEADERS='HTTP/2 200
content-length: 4096'
  }
  err_out=\"\$(api_json GET '/app/api/people?filter[email]=synthetic' 2>&1)\" && rc=0 || rc=\$?
  [[ \$rc -ne 0 ]] || { echo 'api_json accepted a truncated 200 body' >&2; exit 1; }
  grep -q 'GET /app/api/people' <<<\"\$err_out\" || { echo \"diagnostic lost the method+path: \$err_out\" >&2; exit 1; }
  grep -q 'HTTP 200' <<<\"\$err_out\" || { echo \"diagnostic lost the status: \$err_out\" >&2; exit 1; }
  grep -q 'content-length: 4096' <<<\"\$err_out\" || { echo \"diagnostic lost the content-length: \$err_out\" >&2; exit 1; }
  grep -q 'per_' <<<\"\$err_out\" || { echo \"diagnostic lost the excerpt: \$err_out\" >&2; exit 1; }
  echo 'staging-jev-e2e: api_json diagnoses a truncated 200 body with status, headers, and excerpt.'

  # A complete 2xx body still passes through untouched.
  api() {
    printf '{\"data\":[{\"id\":\"per_1\"}]}\n'
    API_STATUS=200
    API_HEADERS='HTTP/2 200'
  }
  out=\"\$(api_json GET /app/api/people)\" || { echo 'api_json rejected a complete body' >&2; exit 1; }
  [[ \"\$out\" == *per_1* ]] || { echo 'complete-body passthrough failed' >&2; exit 1; }
  echo 'staging-jev-e2e: api_json passes a complete 2xx body through.'

"

# The fourth staging dispatch (run 43877403907) died in seed with
# `curl: (3) bad range in URL position 51`: curl treats [ and ] as a URL glob
# range, so every bracketed query (filter[email]=...) failed client-side with
# exit 3 and no request left the box. api() now passes --globoff. This check
# runs the shipped api() verbatim against a throwaway local server and asserts
# the server received the literal brackets — it fails against the pre-fix
# api(), whose curl globbed the URL and never connected.

awk '/^api\(\) \{/,/^\}/' "$harness" > "${scratch}/api-only.sh"
grep -q -- '--globoff' "${scratch}/api-only.sh"

cat > "${scratch}/server.py" <<'PY'
import http.server
import sys

log_path, port_path = sys.argv[1], sys.argv[2]


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open(log_path, 'a') as fh:
            fh.write(self.path + '\n')
        body = b'{"ok":true}'
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args):
        pass


server = http.server.HTTPServer(('127.0.0.1', 0), Handler)
with open(port_path, 'w') as fh:
    fh.write(str(server.server_port))
server.serve_forever()
PY

python3 "${scratch}/server.py" "${scratch}/requests.log" "${scratch}/port" &
server_pid=$!

for _ in $(seq 1 50); do
  if [[ -s "${scratch}/port" ]]; then break; fi
  sleep 0.1
done
port="$(cat "${scratch}/port")"

bash -euo pipefail -c "
  source '${scratch}/api-only.sh'
  NAVIGATOR_HOST='http://127.0.0.1:${port}'
  NAVIGATOR_TOKEN='test-token'
  api GET '/app/api/people?filter[email]=[synthetic-client-43877403907-1]' >/dev/null
  [[ \"\$API_STATUS\" == 200 ]] || { echo \"bracketed query failed with HTTP \$API_STATUS\" >&2; exit 1; }
"

grep -F 'filter[email]=[synthetic-client-43877403907-1]' "${scratch}/requests.log" > /dev/null \
  || { echo 'the server never received the literal bracketed query' >&2; exit 1; }
echo 'staging-jev-e2e: api passes bracketed queries literally (curl globbing is off).'
