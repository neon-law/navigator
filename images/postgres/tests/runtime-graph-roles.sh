#!/usr/bin/env bash
# Only a fresh, exclusively owned cluster; never a shared PostgreSQL service.
set -euo pipefail
set +x
umask 077
root=$(cd -- "$(dirname -- "$0")/../../.." && pwd)
image=${POSTGRES_IMAGE:?current pinned POSTGRES_IMAGE required}
pin=$(sed -n 's/^[[:space:]]*newName: //p' "$root/k8s/postgres/kustomization.yaml")
digest=$(sed -n 's/^[[:space:]]*digest: //p' "$root/k8s/postgres/kustomization.yaml")
[[ "$image" == "$pin@$digest" && "$digest" =~ ^sha256:[a-f0-9]{64}$ ]]
image_id=$(docker image inspect --format '{{.Id}}' "$image")
nonce=$(openssl rand -hex 12)
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/runtime-graph-roles.XXXXXXXX")
printf 'Private role proof evidence: %s\n' "$work"
mkdir -p "$work/secrets"/{bootstrap,migrator,app,backup,probe}
for role in bootstrap migrator app backup; do openssl rand -hex 24 > "$work/secrets/$role/password"; done
printf '127.0.0.1:5432:*:postgres:%s\n' "$(cat "$work/secrets/bootstrap/password")" > "$work/secrets/probe/admin.pgpass"
printf '127.0.0.1:5432:*:navigator_app:%s\n' "$(cat "$work/secrets/app/password")" > "$work/secrets/probe/app.pgpass"
chmod 600 "$work/secrets/probe/"*.pgpass
container=''
cleanup() {
    result=$?
    trap - EXIT
    if [[ -n "$container" ]]; then
        id=$(docker inspect --format '{{.Id}}' "$container" 2>/dev/null || true)
        owner=$(docker inspect --format '{{index .Config.Labels "codex.operation"}}' "$container" 2>/dev/null || true)
        task=$(docker inspect --format '{{index .Config.Labels "codex.task"}}' "$container" 2>/dev/null || true)
        actual_image=$(docker inspect --format '{{.Image}}' "$container" 2>/dev/null || true)
        mounts=$(docker inspect --format '{{json .Mounts}}' "$container" 2>/dev/null || true)
        id_matches=false; operation_matches=false; task_matches=false; image_matches=false; mounts_match=false
        if [[ "$container" =~ ^[a-f0-9]{64}$ && "$id" == "$container" ]]; then id_matches=true; fi
        if [[ "$owner" == "$nonce" ]]; then operation_matches=true; fi
        if [[ "$task" == runtime-graph-roles ]]; then task_matches=true; fi
        if [[ "$actual_image" == "$image_id" ]]; then image_matches=true; fi
        if python3 "$root/images/postgres/tests/runtime-graph-mounts.py" "$owned_mounts" "$mounts"; then mounts_match=true; fi
        if $id_matches && $operation_matches && $task_matches && $image_matches && $mounts_match; then
            docker logs --tail 150 "$container" > "$work/postgres.log" 2>&1 || true
            docker rm -f -v "$container" > "$work/cleanup.log" 2>&1 || result=1
        else
            printf 'Ownership/volume uncertainty: id=%s operation=%s task=%s image=%s mounts=%s; retaining captured ID; no cleanup.\n' \
                "$id_matches" "$operation_matches" "$task_matches" "$image_matches" "$mounts_match" >&2
            result=1
        fi
    fi
    printf 'Role proof exit=%s evidence=%s\n' "$result" "$work"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
owned_mounts=''
container=$(docker create --pull=never --user 0 --name "runtime-graph-roles-$nonce" \
    --label codex.task=runtime-graph-roles --label "codex.operation=$nonce" \
    -p 127.0.0.1::5432 \
    --mount "type=bind,source=$root,target=/probe,readonly" \
    --mount "type=bind,source=$work/secrets,target=/etc/navigator/postgres,readonly" \
    --tmpfs /var/lib/postgresql/data:rw,size=1073741824 \
    -e PGDATA=/var/lib/postgresql/data/owned -e POSTGRES_DB=postgres -e POSTGRES_USER=postgres \
    -e POSTGRES_PASSWORD_FILE=/etc/navigator/postgres/bootstrap/password \
    "$image" postgres -c shared_preload_libraries=age -c ssl=off -c max_connections=30 -c shared_buffers=64MB)
[[ "$container" =~ ^[a-f0-9]{64}$ ]]
owned_mounts=$(docker inspect --format '{{json .Mounts}}' "$container")
printf '%s\n%s\n' "$container" "$owned_mounts" > "$work/ownership.log"
docker start "$container" > "$work/start.log"
ready=false
for ((attempt=0; attempt<90; attempt++)); do
    if docker exec --user 0 "$container" pg_isready -q -h 127.0.0.1 -U postgres -d postgres; then ready=true; break; fi
    sleep 1
done
[[ "$ready" == true ]]
port=$(docker port "$container" 5432/tcp)
[[ "$port" =~ ^127\.0\.0\.1:([0-9]+)$ ]]
export GRAPH_ROLE_PORT=${BASH_REMATCH[1]} GRAPH_ROLE_CONTAINER=$container GRAPH_ROLE_SECRET_DIR="$work/secrets"
export NAVIGATOR_FORBID_SURREAL=1
cd "$root"
cargo run --locked -p store --features test-support --example runtime_graph_roles 2>&1 | tee "$work/role-proof.log"
