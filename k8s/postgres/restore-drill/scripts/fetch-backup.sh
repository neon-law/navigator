#!/bin/bash
# Download one completed base backup (the newest unless RESTORE_BASE_BACKUP
# names a UTC stamp) and the archived WAL into the drill's scratch claim.
set -euo pipefail
root="gs://${BACKUP_BUCKET}/${POD_NAMESPACE}/navigator-postgres"
base="${RESTORE_BASE_BACKUP:-latest}"
if [ "$base" = latest ]; then
  base="$(gcloud storage ls "$root/base/*/_COMPLETE" \
    | sed -E 's#.*/base/([^/]+)/_COMPLETE$#\1#' | sort | tail -n 1)"
fi
if [ -z "$base" ]; then
  echo "no completed base backup under $root/base" >&2
  exit 1
fi
gcloud storage cat "$root/base/$base/_COMPLETE" > /dev/null
mkdir -p /restore/base /restore/wal
gcloud storage cp --recursive --quiet "$root/base/$base/*" /restore/base/
if gcloud storage ls "$root/wal/" > /dev/null 2>&1; then
  gcloud storage rsync --quiet "$root/wal" /restore/wal
fi
# The Cloud SDK and PostgreSQL images use different named UIDs. The pod's
# shared fsGroup owns these directories; recovery must read the downloads
# and create a finished-segment copy beside an in-progress WAL segment.
chmod -R g+rX /restore/base /restore/wal
chmod g+w /restore/wal
printf '%s\n' "$base" > /restore/base-stamp
echo "fetched base backup $base and $(find /restore/wal -type f | wc -l) archived WAL file(s)"
