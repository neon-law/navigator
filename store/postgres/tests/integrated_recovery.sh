#!/usr/bin/env bash
# Synthetic application/image/DBOS recovery on the caller's isolated KIND fixture.
set -euo pipefail
context=${1:?KIND context required}
dbos_binary=${2:?pinned upstream DBOS test executable required}
port=${3:-25473}
[[ "$context" == kind-* && "$port" =~ ^[0-9]+$ && -x "$dbos_binary" ]]
kubectl_cmd=(kubectl --context "$context" -n navigator)
target=$(kubectl kustomize k8s/overlays/postgres-spike | awk '/image:/ {print $2}')
base=$(awk '/^FROM / {print $2; exit}' images/postgres/Containerfile)
[[ "$target" =~ @sha256:[a-f0-9]{64}$ && "$base" =~ @sha256:[a-f0-9]{64}$ ]]
scratch=$(mktemp -d)
worker_pid=''
forward_pid=''
source_created=0
restore_created=0
physical_started=0
physical_created=0
cleanup() {
    task_status=$?
    trap - EXIT
    [[ -z "$worker_pid" ]] || kill "$worker_pid" 2>/dev/null || true
    [[ -z "$forward_pid" ]] || kill "$forward_pid" 2>/dev/null || true
    [[ -z "$forward_pid" ]] || wait "$forward_pid" 2>/dev/null || true
    if [[ "$physical_started" == 1 ]]; then
        "${kubectl_cmd[@]}" exec postgres-spike-0 -- pg_ctl -D /tmp/navigator-physical-recovery -m fast -w stop || true
    fi
    if [[ "$physical_created" == 1 ]]; then
        "${kubectl_cmd[@]}" exec postgres-spike-0 -- rm -rf /tmp/navigator-physical-recovery || true
    fi
    "${kubectl_cmd[@]}" set image statefulset/postgres-spike "postgres=$target" >/dev/null || true
    "${kubectl_cmd[@]}" rollout status statefulset/postgres-spike --timeout=60s >/dev/null || true
    if [[ "$source_created" == 1 ]]; then
        "${kubectl_cmd[@]}" exec postgres-spike-0 -- dropdb -U postgres --force compatibility_source || true
    fi
    if [[ "$restore_created" == 1 ]]; then
        "${kubectl_cmd[@]}" exec postgres-spike-0 -- dropdb -U postgres --force compatibility_restore || true
    fi
    if [[ "$task_status" != 0 ]]; then cat "$scratch"/*.log 2>/dev/null || true; fi
    rm -rf "$scratch"
    exit "$task_status"
}
trap cleanup EXIT
sql() {
    "${kubectl_cmd[@]}" exec -i postgres-spike-0 -- psql -U postgres -d "$1" -v ON_ERROR_STOP=1
}
forward() {
    "${kubectl_cmd[@]}" port-forward service/postgres-spike "$port:5432" > "$scratch/forward.log" 2>&1 &
    forward_pid=$!
    for attempt in $(seq 1 30); do
        if (echo > "/dev/tcp/127.0.0.1/$port") 2>/dev/null; then return; fi
        kill -0 "$forward_pid"
        sleep 1
    done
    return 1
}

echo "base image: $base"
echo "target image: $target"
pvc=$("${kubectl_cmd[@]}" get pvc data-postgres-spike-0 -o jsonpath='{.metadata.uid}')
"${kubectl_cmd[@]}" set image statefulset/postgres-spike "postgres=$base"
"${kubectl_cmd[@]}" rollout status statefulset/postgres-spike --timeout=180s
"${kubectl_cmd[@]}" exec postgres-spike-0 -- createdb -U postgres compatibility_source
source_created=1
sql compatibility_source < store/postgres/migrations/20261010120000_create_core_tenancy.sql
sql compatibility_source < store/postgres/migrations/20261010130000_create_post.sql
"${kubectl_cmd[@]}" set image statefulset/postgres-spike "postgres=$target"
"${kubectl_cmd[@]}" rollout status statefulset/postgres-spike --timeout=180s
[[ "$("${kubectl_cmd[@]}" get pvc data-postgres-spike-0 -o jsonpath='{.metadata.uid}')" == "$pvc" ]]
sql compatibility_source < store/postgres/migrations/20261011120000_create_social_handle_and_publication.sql
sql compatibility_source < store/postgres/migrations/20261011200000_create_firm_journal.sql
sql compatibility_source < store/postgres/migrations/20261013130000_add_firm_slug.sql
sql compatibility_source < store/postgres/tests/synthetic_core_seed.sql
sql compatibility_source < store/postgres/tests/integrated_seed.sql
sql compatibility_source < store/postgres/tests/integrated_verify.sql

forward
DBOS_DATABASE_URL="postgresql://postgres:synthetic@127.0.0.1:$port/compatibility_source" \
INJECT_CRASH_WINDOW=1 "$dbos_binary" > "$scratch/dbos-before.log" 2>&1 &
worker_pid=$!
for attempt in $(seq 1 60); do
    count=$("${kubectl_cmd[@]}" exec postgres-spike-0 -- psql -U postgres -d compatibility_source -Atc \
        "SELECT count(*) FROM public.effects WHERE key='business-write'")
    if [[ "$count" == 1 ]]; then break; fi
    kill -0 "$worker_pid"
    sleep 1
done
[[ "$count" == 1 ]]
kill -KILL "$worker_pid"
wait "$worker_pid" 2>/dev/null || true
worker_pid=''
kill "$forward_pid"
wait "$forward_pid" 2>/dev/null || true
forward_pid=''

"${kubectl_cmd[@]}" exec postgres-spike-0 -- pg_dump -U postgres -Fc --no-owner --no-acl \
    compatibility_source > "$scratch/database.dump"
"${kubectl_cmd[@]}" exec postgres-spike-0 -- createdb -U postgres compatibility_restore
restore_created=1
"${kubectl_cmd[@]}" exec -i postgres-spike-0 -- pg_restore -U postgres --exit-on-error \
    --no-owner --no-acl -d compatibility_restore < "$scratch/database.dump"
sql compatibility_restore < store/postgres/tests/integrated_verify.sql
before=$("${kubectl_cmd[@]}" get pod postgres-spike-0 -o jsonpath='{.metadata.uid}')
"${kubectl_cmd[@]}" rollout restart statefulset/postgres-spike
"${kubectl_cmd[@]}" rollout status statefulset/postgres-spike --timeout=180s
[[ "$("${kubectl_cmd[@]}" get pod postgres-spike-0 -o jsonpath='{.metadata.uid}')" != "$before" ]]
[[ "$("${kubectl_cmd[@]}" get pvc data-postgres-spike-0 -o jsonpath='{.metadata.uid}')" == "$pvc" ]]
sql compatibility_restore < store/postgres/tests/integrated_verify.sql
forward
DBOS_DATABASE_URL="postgresql://postgres:synthetic@127.0.0.1:$port/compatibility_restore" \
    "$dbos_binary" > "$scratch/dbos-after.log" 2>&1 &
worker_pid=$!
for attempt in $(seq 1 60); do
    if ! kill -0 "$worker_pid" 2>/dev/null; then break; fi
    sleep 1
done
if kill -0 "$worker_pid" 2>/dev/null; then exit 1; fi
wait "$worker_pid"
worker_pid=''
sql compatibility_restore <<'SQL'
DO $$
BEGIN
    IF (SELECT count(*) FROM public.attempts WHERE key='checkpoint-one') <> 1 THEN
        RAISE EXCEPTION 'completed DBOS checkpoint replayed';
    END IF;
    IF (SELECT count(*) FROM public.attempts WHERE key='business-write') <> 2 THEN
        RAISE EXCEPTION 'DBOS did not retry the interrupted business step';
    END IF;
    IF (SELECT count(*) FROM public.effects) <> 2 THEN
        RAISE EXCEPTION 'a business effect duplicated';
    END IF;
    IF NOT EXISTS (SELECT 1 FROM dbos.workflow_status WHERE workflow_uuid='stable-smoke' AND status='SUCCESS') THEN
        RAISE EXCEPTION 'restored DBOS workflow did not complete';
    END IF;
END $$;
SQL
"${kubectl_cmd[@]}" exec postgres-spike-0 -- test ! -e /tmp/navigator-physical-recovery
physical_created=1
"${kubectl_cmd[@]}" exec postgres-spike-0 -- env PGPASSWORD=synthetic \
    pg_basebackup -h 127.0.0.1 -U postgres -D /tmp/navigator-physical-recovery -X stream --checkpoint=fast
physical_started=1
"${kubectl_cmd[@]}" exec postgres-spike-0 -- pg_ctl -D /tmp/navigator-physical-recovery \
    -l /tmp/navigator-physical-recovery/recovery.log -o '-p 55432 -k /tmp' -w start
"${kubectl_cmd[@]}" exec -i postgres-spike-0 -- psql -h 127.0.0.1 -p 55432 -U postgres \
    -d compatibility_restore -v ON_ERROR_STOP=1 < store/postgres/tests/integrated_verify.sql
"${kubectl_cmd[@]}" exec postgres-spike-0 -- psql -h 127.0.0.1 -p 55432 -U postgres \
    -d compatibility_restore -v ON_ERROR_STOP=1 -c \
    "SELECT 1 / count(*) FROM dbos.workflow_status WHERE workflow_uuid='stable-smoke' AND status='SUCCESS'"
echo 'application upgrade, image replacement, AGE/types, logical/physical restore and DBOS checkpoint recovery passed'
