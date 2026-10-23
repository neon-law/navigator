#!/usr/bin/env bash
# Verify one deployment row is wholly on a release tag and publicly healthy.
#
# Read-only: every call is a `kubectl get`, a `curl`, or one SELECT through `kubectl exec` under a
# read-only transaction. Nothing here mutates the cluster, so it is safe to run before a roll to
# capture a baseline and after one to confirm the result.
#
# It re-reads each object rather than trusting the roll's own output, because a batch of
# `kubectl set image` calls can lose one to a transient TLS handshake timeout while the others
# succeed — leaving a row that looks rolled and is not.
#
# Usage, from the repository root:
#   .agents/skills/deploy-staging/scripts/verify-row.sh <neon-law-stg|neon-law-prod> <tag>
set -uo pipefail

ROW="${1:?usage: verify-row.sh <row> <tag>}"
TAG="${2:?usage: verify-row.sh <row> <tag>}"
CFG="deployments/$ROW/config.toml"
[ -f "$CFG" ] || { echo "no $CFG — run from the repository root"; exit 2; }

case "$(uname -s)" in
  Darwin) export PATH="/opt/homebrew/share/google-cloud-sdk/bin:$PATH" ;;
esac
coord() { sed -n "s/^$1 = \"\(.*\)\"\$/\1/p" "$CFG"; }
CTX=$(coord NAVIGATOR_GKE_CONTEXT)
NS=$(coord NAVIGATOR_K8S_NAMESPACE)
HOST=$(coord NAVIGATOR_PUBLIC_HOST)
WF=$(coord NAVIGATOR_WORKFLOWS_HOST)
# Read the registry from the row rather than hardcoding it, so this check grades exactly the images
# `ops ship` renders. An image on any other registry host is reported below, never skipped.
REGISTRY=$(coord NAVIGATOR_IMAGE_REGISTRY)
fail=0
note() { printf '%-6s %s\n' "$1" "$2"; [ "$1" = FAIL ] && fail=1; return 0; }

echo "== $ROW → $TAG (context $CTX)"

# Deployment and CronJob images. Only navigator's own images are pinned to the tag; the envoy
# sidecar, clamav, and the otel collector are versioned independently and must not be flagged.
# A single transient `TLS handshake timeout` mid-read would otherwise yield a short list and a
# misleading all-OK verdict, so read with one retry and refuse to grade a read that errored.
read_images() {
  kubectl --context "$CTX" -n "$NS" get deployments,cronjobs \
    -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{range .spec.template.spec.containers[*]}{.image}{" "}{end}{range .spec.jobTemplate.spec.template.spec.containers[*]}{.image}{" "}{end}{"\n"}{end}' 2>&1
}
imgs=$(read_images)
if [ $? -ne 0 ] || printf '%s' "$imgs" | grep -qiE 'unable to connect|timeout|error|refused'; then
  imgs=$(read_images)
  if [ $? -ne 0 ] || printf '%s' "$imgs" | grep -qiE 'unable to connect|timeout|error|refused'; then
    note FAIL "cannot read the cluster cleanly: $(printf '%s' "$imgs" | tail -1)"
    echo; echo "$ROW NOT verified — the cluster read failed, so nothing below was checked"
    exit 1
  fi
fi

while IFS=$'\t' read -r name images; do
  [ -z "${name:-}" ] && continue
  for i in $images; do
    case "$i" in
      # A release roll pins each image to its signed digest, so `repo@sha256:…` is on the tag when the
      # registry resolves `repo:TAG` to that same digest.
      "$REGISTRY"/*@sha256:*)
        tagged=$(gcloud artifacts docker images describe "${i%@*}:$TAG" \
          --format='value(image_summary.digest)' 2>/dev/null)
        if [ -n "$tagged" ] && [ "${i##*@}" = "$tagged" ]; then note OK "$name → ${i##*/} ($TAG)"
        else note FAIL "$name is on ${i##*@}, not $TAG's digest ${tagged:-unresolved}"; fi ;;
      "$REGISTRY"/*)
        if [ "${i##*:}" = "$TAG" ]; then note OK "$name → ${i##*/}"
        else note FAIL "$name is on ${i##*:}, expected $TAG"; fi ;;
      # Navigator's images all live in the one registry this row declares, so one under a different
      # GHCR or Artifact Registry path is wrong-registry drift, not a third-party image. Third-party
      # images (envoy, clamav, otel) are on neither host and fall through, which is why this tests the
      # host and not the tag.
      ghcr.io/*|*-docker.pkg.dev/*)
        note FAIL "$name is on ${i%%:*}, expected $REGISTRY" ;;
    esac
  done
done <<< "$imgs"

# Pod readiness. Completed CronJob pods are history, not health, so they are excluded.
pods=$(kubectl --context "$CTX" -n "$NS" get pods \
  --field-selector=status.phase!=Succeeded,status.phase!=Failed \
  -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.status.phase}{"\t"}{range .status.containerStatuses[*]}{.ready}{","}{end}{"\n"}{end}' 2>/dev/null)
while IFS=$'\t' read -r p phase ready; do
  [ -z "${p:-}" ] && continue
  case "$p" in clamav*|otel*) continue ;; esac
  if [ "$phase" = Running ] && [[ "$ready" != *false* ]]; then note OK "$p $phase ${ready%,}"
  else note FAIL "$p $phase ready=${ready%,}"; fi
done <<< "$pods"

# Public surface.
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 20 --location "https://$HOST/")
[ "$code" = 200 ] && note OK "https://$HOST/ → 200" || note FAIL "https://$HOST/ → $code"

# The workflows root is a Restate protocol endpoint; it answers 400 to a plain GET. TLS and
# reachability are the check, so any HTTP response passes and only a connection failure fails.
code=$(curl -s -o /dev/null -w '%{http_code}' --max-time 20 "https://$WF/")
[ "$code" != 000 ] && note OK "https://$WF/ → $code (protocol endpoint; TLS reachable)" \
                   || note FAIL "https://$WF/ unreachable"

# GORP Serif. Checked explicitly because `ops ship`'s asset preflight covers the img/ objects and
# not these, so a missing font would otherwise serve a fallback face with nothing reporting it.
for f in GORPSerif-Regular.woff2 GORPSerif-Bold.woff2; do
  read -r code ctype < <(curl -s -o /dev/null -w '%{http_code} %{content_type}' \
    --max-time 20 "https://$HOST/assets/fonts/gorp-serif/$f")
  if [ "$code" = 200 ] && [ "$ctype" = font/woff2 ]; then note OK "$f → 200 $ctype"
  else note FAIL "$f → $code ${ctype:-no-type} (upload to gs://$ROW-assets/fonts/gorp-serif/)"; fi
done

# DBOS Heartbeat. Launch starts the current six-hour UTC slot's run, so after any roll the row
# holds `Heartbeat-<date>T<slot hour>`. Read as the bootstrap superuser inside the pod, under a
# read-only transaction, exactly as docs/dbos-durability.md inspects pending ownership. A run
# still PENDING is reported but does not fail the row: it may be seconds past a slot boundary.
if [ "$(coord NAVIGATOR_DBOS_ENABLED)" = true ]; then
  slot=$(printf '%02d' $(( 10#$(date -u +%H) / 6 * 6 )))
  wf="Heartbeat-$(date -u +%Y-%m-%d)T$slot"
  status=$(kubectl --context "$CTX" -n "$NS" exec navigator-postgres-0 -c postgres -- \
    env PGOPTIONS='-c default_transaction_read_only=on' psql -U postgres -d navigator -Atc \
    "SELECT status FROM dbos.workflow_status WHERE workflow_uuid = '$wf'" 2>/dev/null)
  case "$status" in
    SUCCESS) note OK "DBOS $wf → SUCCESS" ;;
    PENDING|ENQUEUED) note WAIT "DBOS $wf → $status (re-run in a few minutes)" ;;
    *) note FAIL "DBOS $wf → ${status:-no run recorded}" ;;
  esac
else
  note SKIP "DBOS Heartbeat (NAVIGATOR_DBOS_ENABLED is not \"true\")"
fi

echo
[ "$fail" = 0 ] && echo "$ROW verified on $TAG" || echo "$ROW NOT fully verified — see FAIL above"
exit "$fail"
