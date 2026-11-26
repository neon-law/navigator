#!/usr/bin/env bash
# The CLI owns native provisioning. This runner owns only its children and fixture server.
set -Eeuo pipefail
umask 077
# Every child in this native lane must refuse an embedded Source engine.
export NAVIGATOR_FORBID_SURREAL=1
root="$PWD"
private="$(mktemp -d)"
public=/tmp/navigator-native-browser
mkdir -p "$public"
fixture="navigator-browser-fixture-${GITHUB_RUN_ID:?}-${GITHUB_RUN_ATTEMPT:?}"
image_container="navigator-browser-image-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}"
supervisor=''
host_pid=''
fixture_started=false
image_started=false
image_container_id=''
volume_created=false
volume_initializer=''
browser_supervisor=''
volume_operation="$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
repo_volume="navigator-browser-repos-${GITHUB_RUN_ID}-${GITHUB_RUN_ATTEMPT}-${volume_operation}"
startup_started=false
cleanup_failed=false
groups_drained=true
cleanup_complete=false
cleanup_phase=not_started

resources() {
  {
    printf '\nphase: %s\n' "$1"
    if command -v free >/dev/null 2>&1; then free -h; fi
    df -h "$root" /var/lib/docker 2>/dev/null || df -h "$root"
    du -sh "$root/target" /tmp/ci-images 2>/dev/null || true
    docker system df
  } >> "$private/resources.log" 2>&1
}

owned_volume() {
  test "$(docker volume inspect "$repo_volume" --format '{{.Name}}')" = "$repo_volume" &&
    test "$(docker volume inspect "$repo_volume" --format '{{index .Labels "com.neonlaw.navigator.browser-fixture"}}')" = git-runtime &&
    test "$(docker volume inspect "$repo_volume" --format '{{index .Labels "com.neonlaw.navigator.browser-operation"}}')" = "$volume_operation"
}
# Keep the existing 120-second group budget, reserving its final five seconds
# for post-KILL verification. Signal delivery or leader exit alone is not drain.
drain_group() {
  local leader="$1" extra="${2:-}" deadline=$((SECONDS + 120)) forced=false
  while kill -0 -- "-$leader" 2>/dev/null || kill -0 "$leader" 2>/dev/null \
      || { [ -n "$extra" ] && kill -0 "$extra" 2>/dev/null; }; do
    if [ "$SECONDS" -ge "$((deadline - 5))" ] && ! $forced; then
      kill -KILL -- "-$leader" 2>/dev/null || true
      forced=true
      cleanup_failed=true
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then return 1; fi
    sleep 1
  done
  # Both group and tracked PIDs are now absent; only reap a known-exited child.
  wait "$leader" 2>/dev/null || true
}
stop_browser() {
  if [ -z "$browser_supervisor" ]; then return 0; fi
  kill -TERM -- "-$browser_supervisor" 2>/dev/null || true
  if drain_group "$browser_supervisor"; then
    browser_supervisor=''
  else
    cleanup_failed=true
    groups_drained=false
  fi
}

redact() {
  python3 - "$private" "$public" <<'PY'
import json, os, pathlib, re, sys
source, target = map(pathlib.Path, sys.argv[1:])
secrets = [value for key, value in os.environ.items() if value and
           any(word in key for word in ('PASSWORD', 'SECRET', 'TOKEN', 'ACCESS_KEY'))]
# Keep discovered values in this mode-0600 private cache across teardown.
# A second export must not forget credentials when down removes .devx/env.
cache = source / '.redaction-secrets.json'
incomplete = source / '.redaction-incomplete'
if incomplete.exists():
    raise ValueError('An earlier secret-discovery pass was incomplete; retain private logs')
fd = os.open(incomplete, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
os.close(fd)
if cache.exists():
    retained = json.loads(cache.read_text())
    if not isinstance(retained, list) or any(not isinstance(value, str) for value in retained):
        raise ValueError('Private redaction cache is invalid')
    secrets.extend(retained)
# Parse generated assignment values without executing a partly written env file.
# A malformed sensitive assignment refuses publication rather than leaking it.
import shlex
env_file = pathlib.Path('.devx/env')
if env_file.is_file():
    for line in env_file.read_text().splitlines():
        line = line.strip()
        if not line or line.startswith('#'):
            continue
        match = re.fullmatch(r'(?:export\s+)?([A-Z_][A-Z0-9_]*)=(.*)', line)
        sensitive = any(any(word in token for word in ('PASSWORD', 'SECRET', 'TOKEN', 'ACCESS_KEY'))
                        for token in re.findall(r'\b[A-Z_][A-Z0-9_]*\b', line))
        if '=' in line and sensitive and not match:
            raise ValueError('Sensitive generated assignment has unsupported syntax')
        if match and any(word in match[1] for word in ('PASSWORD', 'SECRET', 'TOKEN', 'ACCESS_KEY')):
            values = shlex.split(match[2], comments=False, posix=True)
            if len(values) > 1:
                raise ValueError('Sensitive generated assignment cannot be safely redacted')
            raw = match[2]
            literal = (not values or raw == shlex.quote(values[0]) or
                       re.fullmatch(r"'[^']*'", raw) or
                       re.fullmatch(r'"(?:[^"\\$`]|\\["\\])*"', raw))
            if not literal:
                raise ValueError('Sensitive generated assignment is not a supported literal')
            if values and values[0]:
                secrets.append(values[0])
secrets = sorted(set(secrets), key=len, reverse=True)
fd = os.open(cache, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
with os.fdopen(fd, 'w') as saved:
    os.fchmod(saved.fileno(), 0o600)
    json.dump(secrets, saved)
    saved.flush()
    os.fsync(saved.fileno())
incomplete.unlink()
for path in source.glob('*.log'):
    if path.name.startswith('role-'):
        continue  # Role credentials exist only in the private process, never public raw logs.
    text = path.read_text(errors='replace')
    for value in sorted(secrets, key=len, reverse=True):
        text = text.replace(value, '[REDACTED]')
    text = re.sub(r'([A-Za-z][A-Za-z0-9+.-]*://)[^/\s@]+@', r'\1[USERINFO REDACTED]@', text)
    text = re.sub(r'https?://[^\s?]+\?[^\s]+', '[URL QUERY REDACTED]', text)
    text = re.sub(r'[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]+', '[JWT REDACTED]', text)
    text = '\n'.join(line for line in text.splitlines() if not
        re.search(r'(?i)(?:authorization|set-cookie|cookie):', line))
    (target / path.name).write_text(text)
PY
}
owned_host() {
  python3 - "$supervisor" "$root/target/debug/neon" <<'PY'
import os, pathlib, sys
parent, executable = int(sys.argv[1]), pathlib.Path(sys.argv[2]).resolve()
children = set()
for path in pathlib.Path(f'/proc/{parent}/task').glob('*/children'):
    try:
        children.update(map(int, path.read_text().split()))
    except FileNotFoundError:
        pass
matches = []
for pid in children:
    try:
        status = pathlib.Path(f'/proc/{pid}/status').read_text().splitlines()
        ppid = int(next(line.split()[1] for line in status if line.startswith('PPid:')))
        if ppid == parent and pathlib.Path(f'/proc/{pid}/exe').resolve() == executable and os.getpgid(pid) == parent:
            matches.append(pid)
    except (FileNotFoundError, ProcessLookupError):
        pass
assert len(matches) <= 1, 'More than one owned neon child'
if matches:
    print(matches[0])
PY
}
stop_host() {
  if [ -z "$supervisor" ]; then return 0; fi
  # serve's supervisor drops kill_on_drop children on its own shutdown. Signal
  # the verified app FIRST, allowing its DBOS shutdown before stopping the group.
  current_host="$(owned_host)" || cleanup_failed=true
  if [ -n "$host_pid" ] && [ "$current_host" != "$host_pid" ]; then cleanup_failed=true; fi
  if [ -n "$current_host" ]; then
    kill -TERM "$current_host" 2>/dev/null || cleanup_failed=true
    for _ in $(seq 1 120); do
      if ! kill -0 "$current_host" 2>/dev/null; then break; fi
      sleep 1
    done
    if kill -0 "$current_host" 2>/dev/null; then cleanup_failed=true; fi
  fi
  kill -TERM -- "-$supervisor" 2>/dev/null || true
  if drain_group "$supervisor" "$host_pid"; then
    supervisor=''
    host_pid=''
  else
    cleanup_failed=true
    groups_drained=false
  fi
}

startup_alive() {
  if kill -0 "$supervisor" 2>/dev/null; then return 0; fi
  local startup_status=0
  # This owned child has exited; reaping it cannot wait on a live process.
  wait "$supervisor" || startup_status=$?
  printf '\nQuickstart exited before readiness; child_exit=%s\n' "$startup_status" >> "$private/quickstart.log"
  return 1
}

# Call sites pass fixed phase names only; never command text, argv or environment.
cleanup_checkpoint() {
  cleanup_phase="$1"
  printf 'cleanup_phase=%s\n' "$cleanup_phase" >> "$private/cleanup.log"
}

finalize_cleanup() {
  secondary=$?
  local failure_line="${BASH_LINENO[0]:-0}"
  trap - EXIT INT TERM ERR
  if ! $cleanup_complete; then
    cleanup_failed=true
    printf 'Cleanup interrupted; private supports retained\nprimary_exit=%s\nsecondary_exit=%s\nlast_cleanup_phase=%s\nfailure_line=%s\n' \
      "$result" "$secondary" "${cleanup_phase:-not_started}" "$failure_line" >> "$private/cleanup.log"
  fi
  logs_exported=false
  if redact; then logs_exported=true; else cleanup_failed=true; fi
  if $logs_exported && $cleanup_complete && $groups_drained && ! $image_started; then
    rm -rf "$private"
  fi
  if $cleanup_failed; then
    echo 'Owned native browser cleanup did not complete cleanly' >&2
    if [ "$result" -eq 0 ]; then result=1; fi
  fi
  exit "$result"
}

# Snapshot only our exact image before any teardown command can fail.
snapshot_owned_image() {
  if ! $image_started; then return 0; fi
  test "$(docker inspect "$image_container_id" --format '{{index .Config.Labels "com.neonlaw.navigator.browser-operation"}}')" = "$volume_operation" &&
    test "$(docker inspect "$image_container_id" --format '{{index .Config.Labels "com.neonlaw.navigator.browser-fixture"}}')" = git-runtime || return 1
  docker logs "$image_container_id" > "$private/image.log" 2>&1
}

cleanup() {
  result=$?
  trap - EXIT INT TERM
  trap finalize_cleanup ERR
  # Preserve owned runtime evidence before any teardown or secondary cleanup failure.
  cleanup_checkpoint capture_owned_image
  snapshot_owned_image || cleanup_failed=true
  cleanup_checkpoint export_early_logs
  redact || cleanup_failed=true
  # A later attempt can prove drain; prior cleanup failure remains sticky.
  groups_drained=true
  cleanup_checkpoint drain_browser
  stop_browser
  cleanup_checkpoint drain_host
  stop_host
  if ! $groups_drained; then
    # Metadata only: preserve ownership for diagnostics, never private argv/env.
    printf 'Undrained owned resources; dependent teardown withheld\nbrowser_pgid=%s\nhost_pgid=%s\nhost_pid=%s\nimage_container_id=%s\ninitializer_id=%s\nfixture=%s\nrepository_volume=%s\nvolume_operation=%s\n' \
      "$browser_supervisor" "$supervisor" "$host_pid" "$image_container_id" \
      "$volume_initializer" "$fixture" "$repo_volume" "$volume_operation" >> "$private/cleanup.log"
  fi
  if $groups_drained; then
    if $image_started; then
      cleanup_checkpoint inspect_owned_image
      if test "$(docker inspect "$image_container_id" --format '{{index .Config.Labels "com.neonlaw.navigator.browser-operation"}}')" = "$volume_operation"; then
        image_removable=false
        cleanup_checkpoint stop_owned_image
        if docker stop --time 120 "$image_container_id" >/dev/null \
            && test "$(docker inspect "$image_container_id" --format '{{.State.ExitCode}}')" = 0; then
          image_removable=true
        else cleanup_failed=true
        fi
        cleanup_checkpoint capture_stopped_image
        docker logs "$image_container_id" > "$private/image.log" 2>&1 || cleanup_failed=true
        if $image_removable; then
          cleanup_checkpoint remove_owned_image
          if docker rm "$image_container_id" >/dev/null; then
            image_started=false
          else cleanup_failed=true
          fi
        fi
      else cleanup_failed=true
      fi
    fi
    if [ -n "$volume_initializer" ]; then
      cleanup_checkpoint remove_owned_initializer
      if test "$(docker inspect "$volume_initializer" --format '{{index .Config.Labels "com.neonlaw.navigator.browser-operation"}}')" = "$volume_operation"; then
        docker rm --force "$volume_initializer" >/dev/null || cleanup_failed=true
      else cleanup_failed=true
      fi
    fi
    # The named repository volume survives every app stop/restart. Remove it
    # only after browser descendants and the exact image have drained.
    if $volume_created && ! $image_started; then
      cleanup_checkpoint remove_owned_volume
      if owned_volume; then docker volume rm "$repo_volume" >/dev/null || cleanup_failed=true
      else cleanup_failed=true
      fi
    fi
    if $fixture_started; then
      cleanup_checkpoint capture_owned_fixture
      docker logs "$fixture" > "$private/fixture.log" 2>&1 || cleanup_failed=true
      cleanup_checkpoint stop_owned_fixture
      docker stop "$fixture" >/dev/null || cleanup_failed=true
      cleanup_checkpoint remove_owned_fixture
      docker rm "$fixture" >/dev/null || cleanup_failed=true
    fi
  fi
  if [ -s "$root/.devx/kubeconfig" ]; then
    # Startup can fail before env is written. Inspect only this task's kubeconfig.
    export KUBECONFIG="$root/.devx/kubeconfig"
    cleanup_checkpoint collect_workloads
    kubectl -n navigator get pods,jobs,pvc -o wide > "$private/workloads.log" 2>&1 || true
    cleanup_checkpoint collect_events
    kubectl -n navigator get events --sort-by=.lastTimestamp > "$private/events.log" 2>&1 || true
    cleanup_checkpoint collect_pod_logs
    for pod in $(kubectl -n navigator get pods -o name); do
      printf '\n%s\n' "$pod" >> "$private/pods.log"
      kubectl -n navigator logs "$pod" --all-containers --tail=120 >> "$private/pods.log" 2>&1 || true
      kubectl -n navigator logs "$pod" --all-containers --previous --tail=40 >> "$private/pods.log" 2>&1 || true
    done
    if [ -f .devx/port-forwards.log ]; then
      cleanup_checkpoint copy_forward_logs
      cp .devx/port-forwards.log "$private/forwards.log"
    fi
  fi
  if $startup_started && $groups_drained && ! $image_started; then
    cleanup_checkpoint down_owned_tier
    target/debug/hudson dev worktree-env down --path "$root" >> "$private/cleanup.log" 2>&1 || cleanup_failed=true
  fi
  if ! $groups_drained || $image_started; then
    printf 'Private supports retained\nsupport_directory=%s\nimage_container_id=%s\n' \
      "$private" "$image_container_id" >> "$private/cleanup.log"
  fi
  cleanup_checkpoint finished
  cleanup_complete=true
  finalize_cleanup
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

resources before-image-load
# Artifact identity is separate from host-server execution.
for image in navigator-web neon-server; do
  docker load -i "/tmp/ci-images/$image.tar" > "$private/load-$image.log"
  rm -- "/tmp/ci-images/$image.tar"
done
resources after-image-load
server_image="neon-server:ci-${GITHUB_SHA:?}"
test "$(docker image inspect "$server_image" --format '{{.Id}}')" = \
     "$(docker image inspect "navigator-web:ci-$GITHUB_SHA" --format '{{.Id}}')"
test "$(docker image inspect "$server_image" --format '{{.Config.User}}')" = 'nonroot:nonroot'
docker image inspect "$server_image" --format '{{.Id}}' > "$public/image-id.txt"
mkdir -p "$private/poison"
for engine in restate surreal surrealdb; do
  printf '#!/usr/bin/env bash\ntouch /tmp/navigator-browser-legacy-engine-invoked\nexit 99\n' > "$private/poison/$engine"
  chmod +x "$private/poison/$engine"
done
export PATH="$private/poison:$PATH"
startup_started=true
setsid target/debug/hudson quickstart > "$private/quickstart.log" 2>&1 &
supervisor=$!
for _ in $(seq 1 5400); do
  startup_alive
  if grep -q '^Preview: ' "$private/quickstart.log"; then break; fi
  sleep 1
done
grep -q '^Preview: ' "$private/quickstart.log"
host_pid="$(owned_host)"
test -n "$host_pid"
resources after-quickstart
set -a; source .devx/env; set +a
export KUBECONFIG="$root/.devx/kubeconfig"
base="http://localhost:${PORT:?}"
curl -fsS "$base/app/readyz" >/dev/null
target/debug/hudson ops postgres ready --json > "$private/admission.log" 2>&1
target/debug/hudson dev e2e > "$private/native-smoke.log" 2>&1
curl -fsS "http://localhost:${NAVIGATOR_KIND_FORGEJO_PORT:?}/api/healthz" >/dev/null
for code in sample-litigation sample-transactional sample-estate; do
  test -s ".devx/sample-projects/$code/navigator.yaml"
done

kubectl -n navigator exec navigator-postgres-0 -c postgres -- \
  psql -X -qAt -U postgres -d "$NAVIGATOR_POSTGRES_DATABASE" -v ON_ERROR_STOP=1 -c \
  "SELECT count(DISTINCT p.code) = 3 FROM project p JOIN matter_page m ON m.project_id = p.id WHERE p.code IN ('sample-litigation','sample-transactional','sample-estate') AND p.firm_id IS NOT NULL" | grep -qx t

# Match the signed quickstart's actual operation/checkpoint binding before restart.
python3 - "$private/machine" <<'PY'
import json, os, pathlib, sys, urllib.request, uuid
machine = str(uuid.uuid4())
pathlib.Path(sys.argv[1]).write_text(machine)
request = urllib.request.Request(os.environ['NAVIGATOR_DBOS_INGRESS_URL'] + '/notation/' + machine + '/workflow_start',
    data=json.dumps({'spec_yaml': 'BEGIN:\n  _: END\nEND: {}\n', 'ephemeral': True}).encode(),
    headers={'Authorization': 'Bearer ' + os.environ['NAVIGATOR_DBOS_INGRESS_TOKEN'], 'Content-Type': 'application/json'})
with urllib.request.urlopen(request, timeout=120) as response:
    assert json.load(response)['next_state'] == 'BEGIN'
PY
kubectl -n navigator exec -i navigator-postgres-0 -c postgres -- \
  psql -X -qAt -U postgres -d "$NAVIGATOR_POSTGRES_DATABASE" -v ON_ERROR_STOP=1 \
  --set=machine="$(cat "$private/machine")" <<'SQL' | grep -qx t
SELECT count(*) = 1
FROM dbos.workflow_status w
JOIN dbos.operation_outputs o ON o.workflow_uuid = w.workflow_uuid
  AND o.function_name = 'notation-operation-id' AND o.error IS NULL
JOIN notation_transition t ON t.id::text = (o.output::jsonb #>> '{}')
WHERE w.name = 'NotationTransition' AND w.status = 'SUCCESS'
  AND t.notation_id = :'machine'::uuid AND t.started
  AND t.result->>'next_state' = 'BEGIN';
SQL

resources before-host-drain
# No concurrent executor: stop the host before executing the exact release-image binary.
stop_host
! $cleanup_failed
! curl -fsS "$base/app/readyz" >/dev/null 2>&1
# CLI forwards are separate process groups (orchestrate::detach / Forgejo cluster).
# Verify they survived; never create ad-hoc replacement transports here.
curl -sS --connect-timeout 5 -o /dev/null "$NAVIGATOR_STORAGE_ENDPOINT"
curl -fsS "${OAUTH_ISSUER_URL%/}/.well-known/openid-configuration" >/dev/null
curl -fsS "http://localhost:$NAVIGATOR_KIND_FORGEJO_PORT/api/healthz" >/dev/null
# The exact image reuses the persisted native DBOS operation after host drain.
# A fresh labelled volume is owned by the image UID, not a host checkout.
# Never chmod/chown agent paths or bypass Git ownership checks.
if docker volume inspect "$repo_volume" >/dev/null 2>&1; then
  echo 'Refusing an existing repository volume' >&2
  exit 1
fi
docker volume create --label com.neonlaw.navigator.browser-fixture=git-runtime \
  --label "com.neonlaw.navigator.browser-operation=$volume_operation" "$repo_volume" >/dev/null
owned_volume
volume_created=true
printf '%s\n' "$repo_volume" > "$public/repository-volume.txt"
# Fixture initialization only: no neon, DBOS, database, or provider execution.
volume_initializer="$(docker create --user 0:0 --entrypoint /usr/bin/install \
  --label com.neonlaw.navigator.browser-fixture=git-runtime \
  --label "com.neonlaw.navigator.browser-operation=$volume_operation" \
  --mount "type=volume,source=$repo_volume,target=/tmp/navigator-image-repos" \
  "$server_image" -d -o 65532 -g 65532 -m 700 /tmp/navigator-image-repos)"
docker start --attach "$volume_initializer" > "$private/volume-init.log" 2>&1
test "$(docker inspect "$volume_initializer" --format '{{.State.ExitCode}}')" = 0
docker rm "$volume_initializer" >/dev/null
volume_initializer=''
mkdir -p "$private/samples"
cp -a "${NAVIGATOR_SAMPLE_PROJECTS_DIR:?}/." "$private/samples/"
python3 - "$private/samples" <<'PY'
import pathlib, sys
root = pathlib.Path(sys.argv[1])
for path in [root, *root.rglob('*')]:
    assert not path.is_symlink(), 'Refusing symlinks in synthetic sample mount'
    path.chmod(path.stat().st_mode | (0o055 if path.is_dir() else 0o044))
PY
cp "${NAVIGATOR_POSTGRES_ROOT_CERT:?}" "$private/ca.crt"
chmod 644 "$private/ca.crt"
# Exercise the approved attachment journey against the image and its real scanner.
export NAVIGATOR_DBOS_INBOX_ATTACHMENT_FILING_ENABLED=true
image_env=()
while read -r key; do
  case "$key" in
    NAVIGATOR_RELEASE_TAG|NAVIGATOR_GIT_SHA|NAVIGATOR_BUILD_TIME|NAVIGATOR_PUBLIC_DIR|DIOXUS_PUBLIC_PATH|NAVIGATOR_WORKSHOPS_DIR|NAVIGATOR_POSTGRES_ROOT_CERT|NAVIGATOR_DBOS_ROOT_CERT|NAVIGATOR_SAMPLE_PROJECTS_DIR|NAVIGATOR_GIT_REPO_ROOT|NAVIGATOR_FAKE_SIGNATURE_DIR|CI|NAVIGATOR_FORBID_SURREAL) continue ;;
  esac
  image_env+=(--env "$key")
done < <(sed -nE 's/^([A-Z_][A-Z0-9_]*)=.*/\1/p' .devx/env)
image_container_id="$(docker run -d --name "$image_container" --network host \
  --label com.neonlaw.navigator.browser-fixture=git-runtime \
  --label "com.neonlaw.navigator.browser-operation=$volume_operation" \
  "${image_env[@]}" --env NAVIGATOR_DBOS_INBOX_ATTACHMENT_FILING_ENABLED --env CI=1 --env NAVIGATOR_FORBID_SURREAL=1 \
  --env NAVIGATOR_PUBLIC_DIR=/app/public --env DIOXUS_PUBLIC_PATH=/app/public/dioxus \
  --env NAVIGATOR_WORKSHOPS_DIR=/app/content/workshops \
  --env NAVIGATOR_POSTGRES_ROOT_CERT=/native/ca.crt --env NAVIGATOR_DBOS_ROOT_CERT=/native/ca.crt \
  --env NAVIGATOR_SAMPLE_PROJECTS_DIR=/native/sample-projects \
  --env NAVIGATOR_GIT_REPO_ROOT=/tmp/navigator-image-repos \
  --env NAVIGATOR_FAKE_SIGNATURE_DIR=/tmp/navigator-image-repos/.fake-signature \
  --mount "type=bind,source=$private/ca.crt,target=/native/ca.crt,readonly" \
  --mount "type=bind,source=$private/samples,target=/native/sample-projects,readonly" \
  --mount "type=volume,source=$repo_volume,target=/tmp/navigator-image-repos" \
  "$server_image")"
[[ "$image_container_id" =~ ^[a-f0-9]{64}$ ]]
image_started=true
printf '%s\n' "$image_container_id" > "$public/container-id.txt"
for _ in $(seq 1 300); do
  test "$(docker inspect "$image_container_id" --format '{{.State.Running}}')" = true
  if curl -fsS "$base/app/readyz" >/dev/null; then break; fi
  sleep 1
done
curl -fsS "$base/app/readyz" >/dev/null
python3 - "$base/version" "$public/version.json" <<'PY'
import json, os, pathlib, sys, urllib.request
assert os.environ['EXPECTED_IMAGE_RELEASE'], 'Expected CI image identity is missing'
with urllib.request.urlopen(sys.argv[1], timeout=10) as response:
    version = json.load(response)
assert version['release'] == os.environ['EXPECTED_IMAGE_RELEASE']
assert version['commit_full'] == os.environ['GITHUB_SHA']
pathlib.Path(sys.argv[2]).write_text(json.dumps(version, sort_keys=True))
PY
target/debug/hudson ops assets verify --base-url "$base/public" > "$private/image-assets.log" 2>&1
# Same pinned digest and readiness boundary as the covered native fixture lane.
pg_image="$(awk '/newName:/ {name=$2} /digest: sha256:/ {print name "@" $2; exit}' k8s/postgres/kustomization.yaml)"
test -n "$pg_image"
docker run -d --name "$fixture" --shm-size=256m -p 127.0.0.1::5432 \
  -e POSTGRES_PASSWORD=synthetic -e POSTGRES_DB=smoke "$pg_image" >/dev/null
fixture_started=true
ready=false
for _ in $(seq 1 60); do
  if docker exec "$fixture" pg_isready -U postgres -d smoke >/dev/null \
      && docker exec -i "$fixture" psql -U postgres -d smoke -v ON_ERROR_STOP=1 < store/postgres/schema.sql > "$private/fixture-schema.log" 2>&1; then
    ready=true; break
  fi
  sleep 1
done
$ready
fixture_port="$(docker port "$fixture" 5432/tcp | sed -n 's/^127\.0\.0\.1://p')"
[[ "$fixture_port" =~ ^[0-9]+$ ]]
image_id="$(docker image inspect "$server_image" --format '{{.Id}}')"
container_id="$image_container_id"
# An owned process group also contains Nextest/driver descendants on cancellation.
setsid env NAVIGATOR_TEST_POSTGRES_PORT="$fixture_port" NAVIGATOR_TEST_POSTGRES_DATABASE=smoke \
    NAVIGATOR_FORBID_SURREAL=1 \
    NAV_BROWSER_IMAGE_CONTAINER="$container_id" NAV_BROWSER_IMAGE_ID="$image_id" \
    NAV_BROWSER_IMAGE_REPO_ROOT=/tmp/navigator-image-repos \
    target/debug/hudson dev browser-e2e --base-url "$base" > "$private/browser.log" 2>&1 &
browser_supervisor=$!
if wait "$browser_supervisor"; then
  stop_browser
  ! $cleanup_failed
  redact
else
  result=$?; stop_browser; redact; cat "$public/browser.log"; exit "$result"
fi
cat "$public/browser.log"
! grep -q 'skipping browser test:' "$public/browser.log"
python3 scripts/check-browser-results.py target/nextest/ci/junit.xml "$public/browser.log"


resources after-browser-suites
# This is the same persisted operation across host shutdown and exact image boot.
python3 - "$private/machine" <<'PY'
import json, os, pathlib, sys, urllib.request
base = os.environ['NAVIGATOR_DBOS_INGRESS_URL'] + '/notation/' + pathlib.Path(sys.argv[1]).read_text()
def request(suffix, body=None):
    headers = {'Authorization': 'Bearer ' + os.environ['NAVIGATOR_DBOS_INGRESS_TOKEN']}
    data = None
    if body is not None:
        data = json.dumps(body).encode()
        headers['Content-Type'] = 'application/json'
    request = urllib.request.Request(base + suffix, data=data, headers=headers, method='POST')
    with urllib.request.urlopen(request, timeout=120) as response:
        result = json.load(response)
        return result['state'] if suffix == '/workflow_current_state' else result['next_state']
assert request('/workflow_current_state') == 'BEGIN'
assert request('/workflow_signal', {'condition': '_', 'ephemeral': True}) == 'END'
assert request('/workflow_current_state') == 'END'
PY
# Validate actual native workload/image absence, not just omitted environment keys.
test ! -e /tmp/navigator-browser-legacy-engine-invoked
kubectl get deployments,statefulsets,pods --all-namespaces -o json > "$private/workloads.json"
python3 - "$private/workloads.json" <<'PY'
import json, sys
for item in json.load(open(sys.argv[1]))['items']:
    assert not any(word in item['metadata']['name'].lower() for word in ('surreal', 'restate', 'workflows-service'))
    spec = item.get('spec', {})
    spec = spec.get('template', {}).get('spec', spec)
    for container in spec.get('containers', []):
        assert not any(word in container['image'].lower() for word in ('surreal', 'restate', 'workflows-service'))
PY

# Match the signed native quickstart's retired host-port boundary as well.
slot=$((PORT - 20600))
for port in "$((20100 + slot))" "$((20200 + slot))" "$((21200 + slot))"; do
  if timeout 1 bash -c "</dev/tcp/127.0.0.1/$port" 2>/dev/null; then
    echo 'A retired engine port is listening' >&2
    exit 1
  fi
done
