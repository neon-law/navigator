#!/bin/sh
# Take a compressed, self-contained base backup of navigator-postgres. The WAL
# needed to make it consistent streams into the same backup, so a restore
# needs the archived WAL only to roll forward past it.
set -eu
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
rm -rf /backup/base
pg_basebackup --pgdata=/backup/base --format=tar --gzip --wal-method=stream \
  --checkpoint=fast --label="navigator-${stamp}" --verbose
printf '%s\n' "$stamp" > /backup/stamp
