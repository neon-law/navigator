#!/bin/sh
# Restore the fetched base backup into scratch storage, replay archived WAL to
# RECOVERY_TARGET_TIME (or to the end of the archive), promote, and print the
# verification report. The restored server listens only on a private socket:
# it never joins the live service.
set -eu
started="$(date -u +%s)"
pgdata=/restore/pgdata
rm -rf "$pgdata"
mkdir -m 700 "$pgdata"
tar -xzf /restore/base/base.tar.gz -C "$pgdata"
tar -xzf /restore/base/pg_wal.tar.gz -C "$pgdata/pg_wal"
# The newest segment is archived only as `.partial`; recovery reads it under
# its segment name when no finished copy exists.
for partial in /restore/wal/*.partial; do
  [ -e "$partial" ] || continue
  segment="${partial%.partial}"
  [ -e "$segment" ] || cp "$partial" "$segment"
done
touch "$pgdata/recovery.signal"
{
  echo "restore_command = 'cp /restore/wal/%f %p'"
  echo "recovery_target_action = 'promote'"
  if [ -n "${RECOVERY_TARGET_TIME:-}" ]; then
    echo "recovery_target_time = '${RECOVERY_TARGET_TIME}'"
  fi
} >> "$pgdata/postgresql.auto.conf"

pg_ctl --pgdata="$pgdata" --log=/restore/postgres.log --wait --timeout=7200 \
  --options="-c listen_addresses='' -c unix_socket_directories=/tmp -c ssl=off -c archive_mode=off" \
  start || { cat /restore/postgres.log >&2; exit 1; }
until [ "$(psql --host=/tmp --username=postgres --dbname=postgres --no-psqlrc -Atc 'SELECT pg_is_in_recovery()')" = f ]; do
  sleep 5
done
finished="$(date -u +%s)"

psql --host=/tmp --username=postgres --dbname=navigator --no-psqlrc \
  --set=ON_ERROR_STOP=1 --file=/etc/navigator/postgres/drill/verify.sql
if [ "$(psql --host=/tmp --username=postgres --dbname=postgres --no-psqlrc -Atc \
  "SELECT EXISTS (SELECT FROM pg_database WHERE datname = 'forgejo')")" = t ]; then
  echo "restore drill: verifying forgejo database"
  psql --host=/tmp --username=postgres --dbname=forgejo --no-psqlrc \
    --set=ON_ERROR_STOP=1 --file=/etc/navigator/postgres/drill/verify.sql
fi
echo "restore drill: base_backup=$(cat /restore/base-stamp)" \
  "target=${RECOVERY_TARGET_TIME:-end-of-archive}" \
  "recovery_seconds=$((finished - started))"
pg_ctl --pgdata="$pgdata" --mode=fast stop
