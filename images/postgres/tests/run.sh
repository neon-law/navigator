#!/usr/bin/env bash
set -euo pipefail
image=${1:?image reference required}
name="postgres-smoke-${RANDOM}"
volume="${name}-data"
worker=''
cleanup() {
  if [[ -n "$worker" ]]; then kill -KILL "$worker" 2>/dev/null || true; fi
  docker rm -f "$name" >/dev/null 2>&1 || true
  docker volume rm "$volume" >/dev/null 2>&1 || true
}
trap cleanup EXIT
sql() { docker exec "$name" psql -U postgres -d smoke -v ON_ERROR_STOP=1 -Atc "$1"; }
ready() {
  for _ in $(seq 1 60); do
    if docker exec "$name" pg_isready -h 127.0.0.1 -U postgres -d smoke >/dev/null 2>&1; then return; fi
    sleep 1
  done
  docker logs "$name"; return 1
}
docker run -d --name "$name" -e POSTGRES_PASSWORD=synthetic -e POSTGRES_DB=smoke -v "$volume:/var/lib/postgresql/data" -p 127.0.0.1::5432 "$image"
ready
[[ $(docker exec "$name" stat -c %U /proc/1) == postgres ]]
docker exec -i "$name" psql -U postgres -d smoke < tests/extensions.sql
docker restart "$name" >/dev/null
ready
docker exec -i "$name" psql -U postgres -d smoke < tests/persistence.sql
sql 'CREATE TABLE effects(key text PRIMARY KEY); CREATE TABLE attempts(key text NOT NULL);'
port=$(docker port "$name" 5432/tcp | cut -d: -f2)
export DBOS_DATABASE_URL="postgres://postgres:synthetic@127.0.0.1:${port}/smoke"
cargo build --locked --manifest-path tests/dbos/Cargo.toml
INJECT_CRASH_WINDOW=1 tests/dbos/target/debug/dbos-image-smoke > worker-first.log 2>&1 &
worker=$!
for _ in $(seq 1 90); do
  if [[ $(sql "SELECT count(*) FROM effects WHERE key='business-write'") == 1 ]]; then break; fi
  kill -0 "$worker" || { cat worker-first.log; exit 1; }
  sleep 1
done
[[ $(sql "SELECT count(*) FROM effects WHERE key='business-write'") == 1 ]]
curl -f http://localhost:8080/ready
kill -KILL "$worker"
wait "$worker" 2>/dev/null || true
worker=''
# Recovery uses the same executor identity and application version.
tests/dbos/target/debug/dbos-image-smoke > worker-recovered.log 2>&1 &
worker=$!
for _ in $(seq 1 90); do
  if ! kill -0 "$worker" 2>/dev/null; then break; fi
  sleep 1
done
if kill -0 "$worker" 2>/dev/null; then cat worker-recovered.log; exit 1; fi
wait "$worker" || { cat worker-recovered.log; exit 1; }
worker=''
[[ $(sql 'SELECT count(*) FROM effects') == 2 ]]
[[ $(sql "SELECT count(*) FROM attempts WHERE key='checkpoint-one'") == 1 ]]
[[ $(sql "SELECT count(*) FROM attempts WHERE key='business-write'") == 2 ]]
[[ $(sql "SELECT status FROM dbos.workflow_status WHERE workflow_uuid='stable-smoke'") == SUCCESS ]]
sql 'SELECT workflow_uuid,status,recovery_attempts FROM dbos.workflow_status; SELECT key,count(*) FROM attempts GROUP BY key;'
echo 'PASS: extension, persistent-volume restart, checkpoint replay, crash-window idempotency, Axum readiness'
