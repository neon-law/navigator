#!/usr/bin/env bash
# Exercise DBOS launch recovery across two pods in a disposable namespace.
set -euo pipefail

context=${1:?Kubernetes context required}
worker_binary=${2:?Linux dbos-image-smoke binary required}
[[ -x "$worker_binary" ]]
cluster=(kubectl --context "$context")
image=$("${cluster[@]}" kustomize k8s/overlays/postgres-spike | awk '/image:/ {print $2}')
[[ "$image" =~ ^ghcr\.io/neon-law/navigator/postgres@sha256:[a-f0-9]{64}$ ]]
scratch=$(mktemp -d)
namespace=''
worker_pid=''
cleanup() {
    if [[ -n "$worker_pid" ]]; then
        kill "$worker_pid" 2>/dev/null || true
        wait "$worker_pid" 2>/dev/null || true
    fi
    if [[ -n "$namespace" ]]; then
        "${cluster[@]}" delete namespace "$namespace" --wait=true --timeout=120s
    fi
    rm -r "$scratch"
}
trap cleanup EXIT
new_namespace=$("${cluster[@]}" create -f - -o jsonpath='{.metadata.name}' <<'YAML'
apiVersion: v1
kind: Namespace
metadata:
  generateName: dbos-recovery-
YAML
)
[[ "$new_namespace" =~ ^dbos-recovery-[a-z0-9]+$ ]]
namespace=$new_namespace
kube=("${cluster[@]}" --namespace "$namespace")
"${kube[@]}" apply -f - <<YAML
apiVersion: networking.k8s.io/v1
kind: NetworkPolicy
metadata:
  name: same-namespace
spec:
  podSelector: {}
  policyTypes: [Ingress]
  ingress:
    - from:
        - podSelector: {}
---
apiVersion: v1
kind: Service
metadata:
  name: postgres
spec:
  selector:
    run: postgres
  ports:
    - port: 5432
YAML
"${kube[@]}" run postgres --image="$image" --restart=Never \
    --env=POSTGRES_PASSWORD=synthetic --env=POSTGRES_DB=smoke
"${kube[@]}" wait --for=condition=Ready pod/postgres --timeout=180s

sql() {
    "${kube[@]}" exec postgres -- psql -U postgres -d smoke -v ON_ERROR_STOP=1 -Atqc "$1"
}

for _ in $(seq 1 60); do
    if "${kube[@]}" exec postgres -- pg_isready -h 127.0.0.1 -U postgres -d smoke >/dev/null 2>&1; then break; fi
    sleep 1
done
sql 'CREATE TABLE public.effects (key text PRIMARY KEY); CREATE TABLE public.attempts (key text NOT NULL);'

start_pod() {
    local name=$1
    "${kube[@]}" run "$name" --image="$image" --restart=Never --command -- sleep 600
    "${kube[@]}" wait --for=condition=Ready "pod/$name" --timeout=180s
    "${kube[@]}" exec -i "$name" -- sh -ec 'cat >/tmp/dbos-smoke && chmod 700 /tmp/dbos-smoke' < "$worker_binary"
}

run_worker() {
    local pod=$1
    shift
    "${kube[@]}" exec "$pod" -- env \
        DBOS_SYSTEM_DATABASE_URL=postgresql://postgres:synthetic@postgres:5432/smoke \
        SMOKE_EXECUTOR_ID=worker-before "$@" /tmp/dbos-smoke
}

start_pod worker-before
before_uid=$("${kube[@]}" get pod worker-before -o jsonpath='{.metadata.uid}')
run_worker worker-before INJECT_CRASH_WINDOW=1 > "$scratch/before.log" 2>&1 &
worker_pid=$!
count=0
for _ in $(seq 1 90); do
    count=$(sql "SELECT count(*) FROM public.effects WHERE key='business-write'")
    [[ "$count" == 1 ]] && break
    kill -0 "$worker_pid" || { cat "$scratch/before.log"; exit 1; }
    sleep 1
done
[[ "$count" == 1 ]]
pending=$(sql "SELECT status || ':' || executor_id || ':' || application_version
               FROM dbos.workflow_status WHERE workflow_uuid='stable-smoke'")
[[ "$pending" == PENDING:worker-before:smoke-v1 ]]
checkpoint_sql="SELECT string_agg(function_name, ',' ORDER BY function_id)
                FROM dbos.operation_outputs WHERE workflow_uuid='stable-smoke'"
[[ $(sql "$checkpoint_sql") == checkpoint-one ]]

# Fence the old pod before launching another process under its DBOS identity.
"${kube[@]}" delete pod worker-before --wait=true --timeout=120s
wait "$worker_pid" 2>/dev/null || true
worker_pid=''
[[ -z $("${kube[@]}" get pod worker-before --ignore-not-found -o name) ]]

start_pod worker-after
after_uid=$("${kube[@]}" get pod worker-after -o jsonpath='{.metadata.uid}')
[[ -n "$before_uid" && -n "$after_uid" && "$before_uid" != "$after_uid" ]]
run_worker worker-after RECOVER_EXISTING=1 > "$scratch/after.log" 2>&1 &
worker_pid=$!
for _ in $(seq 1 90); do
    kill -0 "$worker_pid" 2>/dev/null || break
    sleep 1
done
if kill -0 "$worker_pid" 2>/dev/null; then cat "$scratch/after.log"; exit 1; fi
wait "$worker_pid" || { cat "$scratch/after.log"; exit 1; }
worker_pid=''
[[ $(sql "SELECT status || ':' || executor_id || ':' || application_version
          FROM dbos.workflow_status WHERE workflow_uuid='stable-smoke'") == SUCCESS:worker-before:smoke-v1 ]]
[[ $(sql "$checkpoint_sql") == checkpoint-one,business-write,* ]]
[[ $(sql "SELECT count(*) FROM public.attempts WHERE key='checkpoint-one'") == 1 ]]
[[ $(sql "SELECT count(*) FROM public.attempts WHERE key='business-write'") == 2 ]]
[[ $(sql "SELECT count(*) FROM public.effects WHERE key='business-write'") == 1 ]]
echo 'DBOS recovered the existing workflow in a replacement pod with one durable business effect'
