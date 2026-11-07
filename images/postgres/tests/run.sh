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
spatial_sql() { docker exec "$name" psql -U postgres -d spatial -v ON_ERROR_STOP=1 -Atc "$1"; }
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
# The image declares what it ships; the test proves the extensions match.
description=$(docker image inspect -f '{{index .Config.Labels "org.opencontainers.image.description"}}' "$image")
declared=('' '' '')
if [[ "$description" =~ ^PostgreSQL\ ([0-9]+)\.([0-9]+)\ with\ Apache\ AGE\ ([0-9.]+)\ and\ pgvector\ ([0-9.]+), ]]; then
  declared=("$((BASH_REMATCH[1] * 10000 + BASH_REMATCH[2]))" "${BASH_REMATCH[3]}" "${BASH_REMATCH[4]}")
fi
docker exec -i "$name" psql -U postgres -d smoke -v ON_ERROR_STOP=1 -v server_version_num="${declared[0]}" \
  -v age="${declared[1]}" -v vector="${declared[2]}" < tests/extensions.sql
postgis_version=''
if [[ "$description" =~ PostGIS\ ([0-9.]+)\+ ]]; then
  postgis_version="${BASH_REMATCH[1]}"
  docker exec "$name" sh -ec 'test -s /usr/share/source/postgis/postgis_*.dsc
    test -s /usr/share/source/postgis/postgis_*.orig.tar.*
    test -s /usr/share/source/postgis/postgis_*.debian.tar.*'
  docker exec "$name" createdb -U postgres spatial
  docker exec -i "$name" psql -U postgres -d spatial < tests/postgis.sql
  [[ $(spatial_sql "SELECT extversion FROM pg_extension WHERE extname = 'postgis'") == "$postgis_version" ]]
elif [[ ${2:-} != --allow-pre-postgis ]]; then
  echo 'Expected a PostGIS image' >&2
  exit 1
fi
docker restart "$name" >/dev/null
ready
docker exec -i "$name" psql -U postgres -d smoke < tests/persistence.sql
if [[ -n "$postgis_version" ]]; then
  [[ $(spatial_sql "SELECT extversion FROM pg_extension WHERE extname = 'postgis'") == "$postgis_version" ]]
  [[ $(spatial_sql "SELECT count(*) FROM smoke_spatial WHERE ST_IsValid(trace) AND ST_SRID(location) = 4326") == 2 ]]
  # Restore the spatial fixture and extension into a fresh database.
  docker exec "$name" createdb -U postgres spatial_restore
  docker exec "$name" pg_dump -U postgres -d spatial -Fc | \
    docker exec -i "$name" pg_restore -U postgres -d spatial_restore --exit-on-error
  [[ $(docker exec "$name" psql -U postgres -d spatial_restore -v ON_ERROR_STOP=1 -Atc \
    "SELECT count(*) FROM smoke_spatial WHERE ST_IsValid(trace) AND ST_SRID(location) = 4326") == 2 ]]
fi
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
