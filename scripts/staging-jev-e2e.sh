#!/usr/bin/env bash
# scripts/staging-jev-e2e.sh — the synthetic LAW-347 staging end-to-end run.
#
# Drives Navigator's own doors over HTTPS against the staging deployment and
# reports what the LAW-347 methods actually decided:
#
#   precheck        — the assign-matches door answers and the gate is closed
#   seed            — synthetic persons and entities through the seed door,
#                     synthetic matters through the open-matter door
#   import-threads  — synthetic threads through the ingest-only inbox import
#                     door; nothing is ever sent (AGENTS.md, "Email is
#                     draft-only")
#   run-assignment  — the assign-matches preview across its cursor, with the
#                     gate open for this run only
#   merge-evidence  — joins the server's `inbox assignment decision` log
#                     (model id, chosen option, confidence, body use) into a
#                     run log the PR attaches
#   cleanup         — deletes the synthetic rows the doors allow and lists
#                     what only a staging reset removes, by id
#
# Every synthetic row carries the run id and the test domain, so anything the
# doors cannot delete is still findable and removable by hand.
#
# Environment: NAVIGATOR_HOST, NAVIGATOR_TOKEN, OUT_DIR (required);
# RUN_ID, SYNTH_DOMAIN, NAMESPACE, WEB_DEPLOYMENT, KUBECTL_BIN optional.
# The bearer is read from the environment and never echoed.

set -euo pipefail

NAVIGATOR_HOST="${NAVIGATOR_HOST:?set NAVIGATOR_HOST, for example https://staging.neonlaw.com}"
NAVIGATOR_TOKEN="${NAVIGATOR_TOKEN:?set NAVIGATOR_TOKEN to a staging operator session bearer}"
OUT_DIR="${OUT_DIR:?set OUT_DIR, the directory the run writes its evidence into}"

RUN_ID="${RUN_ID:-$(date -u +%Y%m%d-%H%M%S)}"
SYNTH_DOMAIN="${SYNTH_DOMAIN:-test.neonlaw}"
NAMESPACE="${NAMESPACE:-neon-law-stg}"
WEB_DEPLOYMENT="${WEB_DEPLOYMENT:-navigator-web}"
KUBECTL_BIN="${KUBECTL_BIN:-kubectl}"

mkdir -p "$OUT_DIR/eml"
STATE="$OUT_DIR/state.json"
VIEWS="$OUT_DIR/views.jsonl"
DECISIONS="$OUT_DIR/decisions.json"
RUN_LOG_MD="$OUT_DIR/run-log.md"
RUN_LOG_JSON="$OUT_DIR/run-log.json"

if [[ ! -f "$STATE" ]]; then
  jq -n --arg run "$RUN_ID" --arg host "$NAVIGATOR_HOST" \
    '{run_id: $run, host: $host, persons: [], entities: [], projects: [],
      messages: [], conversations: [], notes: []}' > "$STATE"
fi

die() { echo "::error::$*" >&2; exit 1; }
note() { echo "[staging-jev-e2e] $*" >&2; }

state_add() { # array_key json_row
  jq --arg key "$1" --argjson row "$2" '.[$key] += [$row]' "$STATE" \
    > "$STATE.tmp" && mv "$STATE.tmp" "$STATE"
}

state_note() {
  jq --arg note "$1" '.notes += [$note]' "$STATE" > "$STATE.tmp" \
    && mv "$STATE.tmp" "$STATE"
}

# One HTTP call against the staging deployment. Prints the body; leaves the
# status in API_STATUS and the response headers in API_HEADERS.
api() {
  local method="$1" path="$2" body="${3:-}" file hdr
  file="$(mktemp)"
  hdr="$(mktemp)"
  if [[ -n "$body" ]]; then
    API_STATUS="$(curl -sS -D "$hdr" -o "$file" -w '%{http_code}' -X "$method" \
      -H "Authorization: Bearer ${NAVIGATOR_TOKEN}" \
      -H 'Content-Type: application/json' \
      --data "$body" \
      "${NAVIGATOR_HOST}${path}")"
  else
    API_STATUS="$(curl -sS -D "$hdr" -o "$file" -w '%{http_code}' -X "$method" \
      -H "Authorization: Bearer ${NAVIGATOR_TOKEN}" \
      "${NAVIGATOR_HOST}${path}")"
  fi
  API_HEADERS="$(tr -d '\r' < "$hdr")"
  cat "$file"
  rm -f "$file" "$hdr"
}

api_json() {
  local out
  out="$(api "$1" "$2" "${3:-}")"
  if [[ "$API_STATUS" -lt 200 || "$API_STATUS" -ge 300 ]]; then
    die "$1 $2 -> HTTP $API_STATUS: $(head -c 400 <<<"$out")"
  fi
  printf '%s' "$out"
}

# Resolve a Person's id by a unique email fragment: the directory door is
# paginated, so a bare listing can miss rows; `filter[email]` is a
# case-insensitive substring match over the whole directory. The run id in
# every synthetic address makes the fragment unique.
person_id_by_email() {
  local fragment="$1" response
  response="$(api_json GET "/app/api/people?filter[email]=${fragment}")"
  jq -r '.data[0].id // empty' <<<"$response"
}
entities_cache() { ENTITIES_CACHE="$(api_json GET /app/api/entities)"; }

# --- precheck -------------------------------------------------------------

precheck() {
  note "precheck: the door must answer and the gate must be closed"

  # Residual gate-open state from an earlier run would invalidate this run's
  # "open only for this run" claim, so the deployment env is read first.
  local env_names
  if env_names="$("$KUBECTL_BIN" get deploy "$WEB_DEPLOYMENT" -n "$NAMESPACE" -o json 2>/dev/null \
      | jq -r '[.spec.template.spec.containers[0].env[]?.name] | join(",")')"; then
    if grep -q "NAVIGATOR_TYPESAFE_ZDR_CONFIRMED" <<<"$env_names"; then
      die "NAVIGATOR_TYPESAFE_ZDR_CONFIRMED is already on ${WEB_DEPLOYMENT} — residual gate-open state; clear it before running"
    fi
    note "precheck: the deployment env carries no gate flag (closed)"
  else
    note "precheck: kubectl could not read the deployment env; the workflow asserts the closed gate instead"
    state_note "precheck could not read the deployment env; the gate residual check is the workflow's kubectl assertion"
  fi

  local probe backlog
  probe="$(api POST /app/api/inbox/assign-matches '{"apply":false,"after":null}')"
  [[ "$API_STATUS" == 200 ]] || die "assign-matches precheck failed (HTTP $API_STATUS): $(head -c 300 <<<"$probe")"
  backlog="$(jq 'length' <<<"$probe")"
  note "precheck: door answered; backlog holds ${backlog} thread(s)"
  jq --argjson backlog "$backlog" '.backlog_before = $backlog' "$STATE" \
    > "$STATE.tmp" && mv "$STATE.tmp" "$STATE"
}

# --- seed -----------------------------------------------------------------

seed() {
  note "seed: synthetic persons, entities, and matters"

  local entity_types entity_type
  entity_types="$(api_json GET /app/api/entity-types)"
  entity_type="$(jq -r '.[0].name // empty' <<<"$entity_types")"
  [[ -n "$entity_type" ]] || die "no entity types on the deployment; seed one first (found: none)"

  local client_email="synthetic-client-${RUN_ID}@${SYNTH_DOMAIN}"
  local party_email="synthetic-party-${RUN_ID}@${SYNTH_DOMAIN}"
  local person_yaml="lookup_fields: [email]
records:
  - email: ${client_email}
    name: Synthetic Client (run ${RUN_ID})
  - email: ${party_email}
    name: Synthetic Party (run ${RUN_ID})"
  local person_report
  person_report="$(api_json POST /app/api/seed \
    "$(jq -nc --arg yaml "$person_yaml" '{model: "person", yaml: $yaml, dry_run: false, overwrite: false}')")"
  note "seed: persons created=$(jq -r .created <<<"$person_report") updated=$(jq -r .updated <<<"$person_report")"
  state_add persons "$(jq -nc --arg email "$client_email" '{email: $email}')"
  state_add persons "$(jq -nc --arg email "$party_email" '{email: $email}')"

  local client_id party_id
  client_id="$(person_id_by_email "synthetic-client-${RUN_ID}")"
  party_id="$(person_id_by_email "synthetic-party-${RUN_ID}")"
  [[ -n "$client_id" && -n "$party_id" ]] || die "the seeded persons did not resolve to ids on the people door"

  local code_suffix
  code_suffix="SYNJ$(tr -d ':-' <<<"$RUN_ID")"
  local matters
  matters="$(jq -n --arg client "$client_id" --arg suffix "$code_suffix" --arg run "$RUN_ID" '[
    {code: ($suffix + "A"), name: ("Synthetic Estate Plan A (run " + $run + ")"),
     description: ("A synthetic revocable living-trust and pour-over will engagement for a married couple, funding two trusts and naming successor trustees (run " + $run + ").")},
    {code: ($suffix + "B"), name: ("Synthetic Eviction Defense B (run " + $run + ")"),
     description: ("A synthetic residential eviction defense: answer to an unlawful detainer complaint, habitability affirmative defenses, rent escrow (run " + $run + ").")},
    {code: ($suffix + "C"), name: ("Synthetic Contract Review C (run " + $run + ")"),
     description: ("A synthetic review of a commercial sublease and services agreement, indemnification and termination limits (run " + $run + ").")}
  ] | map(.client_id = $client)')"

  local row entity_name entity_yaml entity_report entity_json project_body project_row project_id
  for row in $(jq -c '.[]' <<<"$matters"); do
    entity_name="Synthetic Entity ($(jq -r .code <<<"$row"))"
    entity_yaml="lookup_fields: [name, entity_type_id]
records:
  - name: ${entity_name}
    entity_type:
      name: ${entity_type}"
    entity_report="$(api_json POST /app/api/seed \
      "$(jq -nc --arg yaml "$entity_yaml" '{model: "entity", yaml: $yaml, dry_run: false, overwrite: false}')")"
    note "seed: entity for $(jq -r .code <<<"$row") created=$(jq -r .created <<<"$entity_report")"

    entities_cache
    entity_json="$(jq -c --arg name "$entity_name" '[.[] | select((.name // "") == $name)][0] // empty' <<<"$ENTITIES_CACHE")"
    [[ -n "$entity_json" ]] || die "the seeded entity ${entity_name} did not resolve to an id"

    project_body="$(jq -nc --argjson spec "$row" --argjson entity "$entity_json" \
      '{name: $spec.name, code: $spec.code, description: $spec.description,
        client_id: $spec.client_id, entity_id: $entity.id, attestation: true}')"
    project_row="$(api_json POST /app/api/projects "$project_body")"
    project_id="$(jq -r '.id // empty' <<<"$project_row")"
    [[ -n "$project_id" ]] || die "open-matter $(jq -r .code <<<"$row") returned no id: $(head -c 300 <<<"$project_row")"
    note "seed: matter $(jq -r .code <<<"$row") opened (${project_id})"
    state_add entities "$(jq -nc --arg id "$(jq -r .id <<<"$entity_json")" --arg name "$entity_name" '{id: $id, name: $name}')"
    state_add projects "$(jq -nc --arg id "$project_id" --arg code "$(jq -r .code <<<"$row")" '{id: $id, code: $code}')"
  done

  # One seeded party participates in matter A, so one thread assigns through
  # the address-match alone — the pre-model path the Jev bands sit on.
  local project_a role_yaml role_report
  project_a="$(jq -r '.projects[0].code' "$STATE")"
  role_yaml="lookup_fields: [person_id, project_id]
records:
  - person:
      email: ${party_email}
    project:
      code: ${project_a}"
  role_report="$(api_json POST /app/api/seed \
    "$(jq -nc --arg yaml "$role_yaml" '{model: "person_project_role", yaml: $yaml, dry_run: false, overwrite: false}')")"
  note "seed: person_project_role created=$(jq -r .created <<<"$role_report")"
}

# --- import-threads -------------------------------------------------------

eml() { # from_address from_name subject body
  cat <<EOF
From: ${2} <${1}>
To: Synthetic Mailbox <synthetic-mailbox-${RUN_ID}@${SYNTH_DOMAIN}>
Subject: ${3}
Date: $(date -u -R)
Message-ID: <syn-${RUN_ID}-$(echo -n "$1" | md5sum | cut -c1-10)@${SYNTH_DOMAIN}>
Content-Type: text/plain; charset=utf-8

${4}
EOF
}

import-threads() {
  note "import: synthetic threads through the ingest-only import door"
  local mailbox="synthetic-mailbox-${RUN_ID}@${SYNTH_DOMAIN}"

  local from_local from subject body file payload response message_id conversation_id
  # (from_local, subject, body) tuples. Senders and content are synthetic;
  # the door ingests, it never sends.
  while IFS=$'\t' read -r from_local subject body; do
    from="synthetic-${from_local}-${RUN_ID}@${SYNTH_DOMAIN}"
    subject="SYNTHETIC JEV E2E ${RUN_ID}: ${subject}"
    file="$OUT_DIR/eml/${from_local}.eml"
    eml "$from" "Synthetic Sender ${from_local}" "$subject" "$body" > "$file"
    payload="$(base64 -w0 "$file")"
    response="$(api_json POST /app/api/inbox/import \
      "$(jq -nc --arg mailbox "$mailbox" --arg payload "$payload" \
        '{source: "email", mailbox: $mailbox, payload_base64: $payload, dry_run: false}')")"
    message_id="$(jq -r '.imported[0].message_id // empty' <<<"$response")"
    conversation_id="$(jq -r '.imported[0].conversation_id // empty' <<<"$response")"
    [[ -n "$message_id" ]] || die "import of ${file} returned no message id: $(head -c 300 <<<"$response")"
    note "import: ${from_local} -> message ${message_id}"
    state_add messages "$(jq -nc --arg id "$message_id" --arg conv "$conversation_id" \
      --arg from "$from" --arg subject "$subject" --arg kind "$from_local" \
      '{id: $id, conversation_id: $conv, from: $from, subject: $subject, kind: $kind}')"
    state_add conversations "$(jq -nc --arg id "$conversation_id" --arg subject "$subject" '{id: $id, subject: $subject}')"
  done <<'THREADS'
addrmatch	Tax documents for the estate plan	Hello, I am the client's financial advisor forwarding the trust funding schedule for the estate plan matter. Please advise on the next steps.
jev-trust	Trust funding question for the couple	We need to retitle the brokerage account into the living trust before the end of the month; can you confirm the trustee names on the schedule?
jev-eviction	Hearing date for the eviction answer	The court set a hearing next week on the unlawful detainer; does the answer's habitability defense change the rent escrow plan?
jev-sublease	Indemnification limits in the sublease	Please review section 9 of the sublease draft; the indemnification cap reads broader than the services agreement allows.
jev-trustee	Successor trustee resignation	The named successor trustee wants to resign before funding completes; what does the pour-over will say about alternates?
none	Catering order for Friday	Here is the lunch order for the Friday team lunch: four vegetarian sandwiches and two coffees. No legal questions this week.
THREADS
}

# --- run-assignment -------------------------------------------------------

run-assignment() {
  note "run: the assign-matches preview across its cursor"
  : > "$VIEWS"
  local cursor="" body resp next
  while :; do
    body="$(jq -nc --arg cursor "$cursor" \
      '{apply: false, after: (if $cursor == "" then null else ($cursor | fromjson) end)}')"
    resp="$(api POST /app/api/inbox/assign-matches "$body")"
    [[ "$API_STATUS" == 200 ]] || die "assign-matches failed (HTTP $API_STATUS): $(head -c 300 <<<"$resp")"
    jq -c '.[]' <<<"$resp" >> "$VIEWS"
    # The continuation rides the x-navigator-next-cursor header as a JSON
    # array; empty (or absent) ends the run.
    next="$(awk 'tolower($1) == "x-navigator-next-cursor:" {print substr($0, index($0, ":") + 1)}' <<<"$API_HEADERS" | sed 's/^ *//')"
    [[ -n "$next" ]] || break
    cursor="$next"
  done
  note "run: $(wc -l < "$VIEWS" | tr -d ' ') thread(s) decided in total"
  jq --slurpfile views "$VIEWS" '.views_total = ($views | length)' "$STATE" \
    > "$STATE.tmp" && mv "$STATE.tmp" "$STATE"
}

# --- merge-evidence -------------------------------------------------------

# The server logs one decision per thread (`tracing::info!` carrying the
# DecisionLog JSON). Pull the deployment's recent pod logs when this run's
# identity may read them; the Jev fill inside carries model id, chosen
# option, confidence, the full distribution, and whether the body excerpt
# was used.
extract_decisions() {
  local since_seconds="$1" log_lines line json collected="[]"
  if "$KUBECTL_BIN" get deploy "$WEB_DEPLOYMENT" -n "$NAMESPACE" >/dev/null 2>&1; then
    log_lines="$("$KUBECTL_BIN" logs deploy/"$WEB_DEPLOYMENT" -n "$NAMESPACE" --since="${since_seconds}s" 2>/dev/null || true)"
    if [[ -z "$log_lines" ]]; then
      state_note "decision-log pull: pod logs were empty or the pods/log read was refused; the Jev detail in this report is partial"
    fi
    while IFS= read -r line; do
      [[ "$line" == *"inbox assignment decision"* ]] || continue
      # -R reads each line as a string; fromjson then parses the embedded
      # DecisionLog. Plain jq would try to parse the whole log line as JSON
      # and fail before the filter ran.
      json="$(sed 's/.*{"method/{"method/' <<<"$line" \
        | jq -Rc 'try fromjson | select(.method == "address-match")' 2>/dev/null || true)"
      if [[ -n "$json" ]]; then
        collected="$(jq -c --argjson d "$json" '. + [$d]' <<<"$collected")"
      fi
    done <<<"$log_lines"
  else
    state_note "decision-log pull: kubectl could not read the deployment; the Jev detail in this report is partial"
  fi
  printf '%s' "$collected"
}

merge-evidence() {
  note "evidence: merge the decision log into the run log"
  local decisions_json
  decisions_json="$(extract_decisions "${DECISION_WINDOW_SECONDS:-900}")"
  printf '%s' "$decisions_json" > "$DECISIONS"

  # Per synthetic thread: the door's decision plus, when the model ran, the
  # decision log's fill. Threads this run did not seed stay out of the report.
  local evidence
  evidence="$(jq -c -s --slurpfile msgs <(jq '.messages' "$STATE") --argjson decisions "$decisions_json" '
    map(select(.message as $id | any($msgs[0][]; .id == $id)))
    | map(. as $v |
        ($decisions | map(select(.message == $v.message))[0]) as $log |
        {message: $v.message,
         conversation_id: $v.conversation_id,
         from: $v.from,
         subject: $v.subject,
         decision: $v.decision,
         band: (if $v.decision.decision == "assigned" then "auto-assign (>= 0.9)"
                elif $v.decision.decision == "proposed" then "propose (0.6 - 0.9)"
                else "inbox with logged reason: \($v.decision.reason // "address-match")" end),
         model_id: ($log.model_id // null),
         chosen: ($log.probabilities.chosen // null),
         confidence: ($log.probabilities.confidence // null),
         body_used: ($log.probabilities.body_used // null),
         probabilities: ($log.probabilities.probabilities // null)})' "$VIEWS")"

  local typesafe bands
  typesafe="$(jq -nc --argjson decisions "$decisions_json" '
    # A thread the Jev method actually decided carries the fill model id;
    # a gate refusal or service failure leaves model_id null but keeps a
    # jev_-spelled reason. The denominator is threads the model attempted.
    ($decisions | map(select(.model_id != null))) as $ok |
    ($decisions
     | map(select(.model_id != null
                  or ((.reason | strings) | startswith("jev_"))))) as $attempted |
    {first_live_call_authenticated: (if ($ok | length) > 0 then "yes — a Jev answer was recorded" else "not proven by this run" end),
     successful_model_calls: ($ok | length),
     model_calls_total: ($attempted | length)}')"
  bands="$(jq -nc --argjson evidence "$evidence" '{
    assigned_0_9: ($evidence | map(select(.decision.decision == "assigned")) | length),
    proposed_0_6_0_9: ($evidence | map(select(.decision.decision == "proposed")) | length),
    inbox_with_logged_reason: ($evidence | map(select(.decision.decision == "left_in_inbox")) | length)}')"

  jq -n --argjson state "$(cat "$STATE")" --argjson evidence "$evidence" \
    --argjson typesafe "$typesafe" --argjson bands "$bands" \
    '{run: $state, typesafe_tenant: $typesafe, bands: $bands, threads: $evidence}' > "$RUN_LOG_JSON"

  {
    echo "# LAW-347 staging end-to-end run — ${RUN_ID}"
    echo
    echo "- Host: \`${NAVIGATOR_HOST}\`; namespace \`${NAMESPACE}\`, deployment \`${WEB_DEPLOYMENT}\`"
    echo "- Backlog before the run: $(jq -r '.backlog_before // "?"' "$STATE") thread(s); synthetic threads imported: $(jq '.messages | length' "$STATE")"
    echo
    echo "## First live call against the staging TypeSafe tenant"
    echo
    echo "- Key authenticated and tenant reachable: **$(jq -r .first_live_call_authenticated <<<"$typesafe")**"
    echo "- Model calls answered: $(jq -r .successful_model_calls <<<"$typesafe") of $(jq -r .model_calls_total <<<"$typesafe")"
    echo "- A 401/403 here is a finding, not a masked failure: the decision-log lines behind this table carry the exact error."
    echo
    echo "## Outcomes across the confidence bands"
    echo
    echo "| Band | Threads |"
    echo "|---|---|"
    echo "| auto-assign (>= 0.9) | $(jq -r .assigned_0_9 <<<"$bands") |"
    echo "| propose (0.6 - 0.9) | $(jq -r .proposed_0_6_0_9 <<<"$bands") |"
    echo "| inbox with logged reason | $(jq -r .inbox_with_logged_reason <<<"$bands") |"
    echo
    echo "## Per-thread outcomes"
    echo
    echo "| Subject | Sender | Band | Model | Chosen | Confidence | Body used | Reason |"
    echo "|---|---|---|---|---|---|---|---|"
    jq -r '.[] | "| \(.subject | sub("SYNTHETIC JEV E2E [0-9-]+: "; "")) | \(.from) | \(.band) | \(.model_id // "—") | \(.chosen // "—") | \(.confidence // "—") | \(.body_used // "—") | \(.decision.reason // "—") |"' <<<"$evidence"
    echo
    echo "## Notes"
    echo
    jq -r '.notes[] | "- \(.)"' "$STATE"
    echo
    echo "- The assign-matches door is preview-only: every outcome above is a recorded decision, not an applied filing; applying one needs per-message lawyer confirmation."
  } > "$RUN_LOG_MD"

  note "evidence: run log written to ${RUN_LOG_MD}"
  cat "$RUN_LOG_MD"
}

# --- cleanup --------------------------------------------------------------

cleanup() {
  note "cleanup: delete the synthetic rows the doors allow"
  local report="" status id

  while IFS= read -r id; do
    [[ -n "$id" ]] || continue
    api DELETE "/app/api/projects/${id}" >/dev/null || true
    status="$API_STATUS"
    report+="- matter ${id}: HTTP ${status}"$'\n'
  done < <(jq -r '.projects[].id' "$STATE")

  # The import door may have created Person rows for the synthetic senders;
  # every synthetic person this run touched is deleted, whatever seeded it.
  # The lookup goes through the filtered directory: the run id in each local
  # part makes the fragment unique.
  local person_email person_id
  while IFS= read -r person_email; do
    [[ -n "$person_email" ]] || continue
    person_id="$(person_id_by_email "${person_email%%@*}")"
    if [[ -z "$person_id" ]]; then
      report+="- person ${person_email}: already gone"$'\n'
      continue
    fi
    api DELETE "/app/api/people/${person_id}" >/dev/null || true
    status="$API_STATUS"
    report+="- person ${person_email} (${person_id}): HTTP ${status}"$'\n'
  done < <({ jq -r '.persons[].email' "$STATE"; jq -r '.messages[].from' "$STATE"; } | sort -u)

  while IFS= read -r id; do
    [[ -n "$id" ]] || continue
    api DELETE "/app/api/entities/${id}" >/dev/null || true
    status="$API_STATUS"
    report+="- entity ${id}: HTTP ${status}"$'\n'
  done < <(jq -r '.entities[].id' "$STATE")

  # Conversations have no delete door: they stay, findable by their subject
  # marker, until the next staging reset.
  while IFS=$'\t' read -r id subject; do
    report+="- conversation ${id} (${subject}): no delete door; removable at the next staging reset"$'\n'
  done < <(jq -r '.conversations[] | [.id, .subject] | @tsv' "$STATE")

  if [[ -f "$RUN_LOG_MD" ]]; then
    {
      echo
      echo "## Cleanup record"
      echo
      printf '%s' "$report"
    } >> "$RUN_LOG_MD"
  fi
  note "cleanup record:"
  printf '%s\n' "$report"
}

# --- dispatch -------------------------------------------------------------

case "${1:-}" in
  precheck) precheck ;;
  seed) seed ;;
  import-threads) import-threads ;;
  run-assignment) run-assignment ;;
  merge-evidence) merge-evidence ;;
  cleanup) cleanup ;;
  *)
    die "usage: $0 {precheck|seed|import-threads|run-assignment|merge-evidence|cleanup}"
    ;;
esac
