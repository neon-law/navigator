#!/bin/bash
# Copy each finished WAL segment to the deployment's backup bucket, then drop
# it from the spool. The in-progress `.partial` segment is re-copied whenever
# it changes, so the archive trails the server by at most one interval.
# A failed copy exits the container; the spool keeps the segment for the retry.
set -euo pipefail
shopt -s nullglob
destination="gs://${BACKUP_BUCKET}/${POD_NAMESPACE}/navigator-postgres/wal"
interval="${UPLOAD_INTERVAL_SECONDS:-60}"
last_partial=""
while true; do
  for segment in /wal/spool/???????????????????????? /wal/spool/*.history; do
    gcloud storage cp --no-clobber --quiet "$segment" "$destination/"
    rm -f -- "$segment"
  done
  for partial in /wal/spool/*.partial; do
    digest="$(basename "$partial") $(md5sum < "$partial")"
    if [ "$digest" != "$last_partial" ]; then
      gcloud storage cp --quiet "$partial" "$destination/"
      last_partial="$digest"
    fi
  done
  date -u +%s > /wal/last-upload
  sleep "$interval"
done
