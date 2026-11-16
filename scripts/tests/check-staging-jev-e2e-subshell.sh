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


# The sixth staging dispatch (runs 43885192582 and 43893047358) died in seed
# with 'jq: parse error: Unfinished string at EOF at line 2, column 0',
# exit 5. Root cause is harness shell, not the server: seed() iterated the
# matter rows with an unquoted `for row in $(jq -c '.[]' ...)`, which
# word-splits each compact JSON row on IFS whitespace — and every synthetic
# matter name and description contains spaces ('Synthetic Estate Plan A (run
# ...)'). The first iteration bound an unfinished fragment and jq died
# parsing it from a herestring. json_array_rows() now does the split with
# mapfile over a process substitution, one complete row per element. This
# check runs the shipped helper verbatim and asserts rows with multi-word
# names and parenthesized run ids iterate as complete JSON documents.

awk '/^json_array_rows\(\) \{/,/^\}/' "$harness" > "${scratch}/rows-only.sh"
grep -q 'mapfile -t' "${scratch}/rows-only.sh"

bash -euo pipefail -c "
  source '${scratch}/rows-only.sh'
  # The same shape seed() builds: multi-word names, parenthesized run ids,
  # and descriptions full of spaces and commas.
  matters='[{\"code\":\"SYNJ0BA\",\"name\":\"Synthetic Estate Plan A (run 43885192582)\",\"description\":\"A synthetic revocable living-trust and pour-over will engagement for a married couple, funding two trusts (run 43885192582).\"},
 {\"code\":\"SYNJ0BB\",\"name\":\"Synthetic Eviction Defense B (run 43885192582)\",\"description\":\"A synthetic residential eviction defense: answer, habitability defenses, rent escrow (run 43885192582).\"},
 {\"code\":\"SYNJ0BC\",\"name\":\"Synthetic Contract Review C (run 43885192582)\",\"description\":\"A synthetic review of a commercial sublease, indemnification and termination limits (run 43885192582).\"}]'
  matter_rows=()
  json_array_rows matter_rows \"\$matters\"
  [[ \${#matter_rows[@]} -eq 3 ]] || { echo 'row count wrong' >&2; exit 1; }
  for row in \"\${matter_rows[@]}\"; do
    # Every element must be a complete JSON document — the pre-fix loop
    # bound fragments like '{\"code\":\"SYNJ0B\",\"name\":\"Synthetic' that
    # died here with 'Unfinished string at EOF'.
    jq -e '.code and .name and .description' >/dev/null 2>&1 <<<\"\$row\" \
      || { echo \"row is not a complete JSON document: \$row\" >&2; exit 1; }
  done
  [[ \"\$(jq -r '.[0].name' <<<\"\$(printf '[%s]' \"\${matter_rows[1]}\")\")\" == 'Synthetic Eviction Defense B (run 43885192582)' ]] \
    || { echo 'row content was mangled by the split' >&2; exit 1; }
"

# The seventh staging dispatch (run 43916737157) got the entities POST for
# the first time (#822's row fix held) and was refused by the server with a
# clean 422: the entity_type block declared no jurisdiction, and the seed
# contract (ENG-518) refuses an entity without one rather than defaulting
# it. seed_entities resolves `entity_type.jurisdiction.name` against an
# EXISTING Jurisdiction — it never creates one — and the canonical
# jurisdictions (store/seeds/Jurisdiction.yaml) are seeded before entities
# on every boot, so the template must name one of those verbatim. This
# check extracts the shipped template helper verbatim and asserts the
# jurisdiction block is present, structurally valid YAML, and names a
# jurisdiction the canonical seed actually ships.

awk '/^matter_entity_yaml\(\) \{/,/^\}/' "$harness" > "${scratch}/yaml-only.sh"
grep -q 'jurisdiction' "${scratch}/yaml-only.sh"

bash -euo pipefail -c "
  source '${scratch}/yaml-only.sh'
  matter_entity_yaml 'Synthetic Entity (SYNJ0BA)' 'LLC' > '${scratch}/entity.yaml'
  python3 - ${scratch}/entity.yaml <<'PY'
import sys

import yaml

with open(sys.argv[1]) as fh:
    doc = yaml.safe_load(fh)
record = doc['records'][0]
entity_type = record['entity_type']
assert 'jurisdiction' in entity_type, f'no jurisdiction block: {entity_type}'
assert entity_type['jurisdiction']['name'] == 'California', entity_type['jurisdiction']
assert record['name'].startswith('Synthetic Entity ('), record['name']
print(f\"jurisdiction block resolves: {entity_type}\")
PY
  grep -qx '  - name: California' store/seeds/Jurisdiction.yaml \
    || { echo 'the declared jurisdiction is not in the canonical seed' >&2; exit 1; }
"

# The eighth staging dispatch (run 43923428002) opened all three synthetic
# matters — the #822 and #825 fixes held — and then the person_project_role
# seed was refused with a clean 422: 'references unknown project code
# "SYNJ439234280021A"'. The seed contract resolves a project by exact code
# match (projects::find_by_code), but open_matter normalizes every accepted
# code to lowercase (normalize_code: trim + to_ascii_lowercase) and
# is_valid_code admits only lowercase letters, digits, and single hyphens —
# so the uppercase fixture code was stored lowercased and the seed's
# uppercase reference could not resolve. This check extracts the shipped
# suffix helper verbatim and asserts its output is already normalized: the
# code the harness sends is the code the server stores.

awk '/^matter_code_suffix\(\) \{/,/^\}/' "$harness" > "${scratch}/code-suffix-only.sh"
grep -q 'tr -d' "${scratch}/code-suffix-only.sh"

bash -euo pipefail -c "
  source '${scratch}/code-suffix-only.sh'
  suffix=\"\$(matter_code_suffix '43923428002-1')\"
  # The output must already satisfy is_valid_code: lowercase letters, digits,
  # and single hyphens only. An uppercase character here is exactly the bug
  # that stranded run 43923428002.
  [[ \"\$suffix\" =~ ^[a-z0-9-]+\$ ]] || {
    echo \"the matter code suffix is not normalized: \${suffix}\" >&2
    exit 1
  }
  [[ \"\$suffix\" == 'synj439234280021' ]] || {
    echo \"unexpected suffix: \${suffix}\" >&2
    exit 1
  }
  ! grep -q 'code_suffix=\"SYNJ' '$harness' || {
    echo 'the harness still builds the code suffix outside the helper' >&2
    exit 1
  }
  echo \"matter code suffix is normalized: \${suffix}\"
"
echo 'staging-jev-e2e: matter codes are emitted normalized for the seed contract.'
echo 'staging-jev-e2e: the entity template declares a canonical jurisdiction.'
echo 'staging-jev-e2e: json_array_rows iterates space-containing JSON rows whole.'
