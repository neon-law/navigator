#!/bin/bash
# Upload the base backup, then write `_COMPLETE` last: a restore only trusts a
# backup whose marker exists.
set -euo pipefail
stamp="$(cat /backup/stamp)"
destination="gs://${BACKUP_BUCKET}/${POD_NAMESPACE}/navigator-postgres/base/${stamp}"
gcloud storage cp --recursive --quiet /backup/base/* "$destination/"
printf '%s\n' "$stamp" | gcloud storage cp --quiet - "$destination/_COMPLETE"
echo "base backup ${stamp} uploaded"
