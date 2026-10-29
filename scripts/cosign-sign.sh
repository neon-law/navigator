#!/usr/bin/env bash
# Keyless-sign one image digest, retrying transient Sigstore failures.
#
#   scripts/cosign-sign.sh <registry>/<image>@sha256:<digest>
#
# A slow Rekor upload can land the transparency-log entry while its response
# is lost; cosign's own retry of that POST then gets `409
# createLogEntryConflict` and exits 1 with nothing attached to the registry.
# Release 26.10.29 failed that way. Each `cosign sign` run mints fresh
# ephemeral keys, so a whole-command retry writes a new entry instead of
# colliding with the old one. Fulcio and registry hiccups retry the same way.
set -euo pipefail

reference=${1:?usage: cosign-sign.sh <registry>/<image>@sha256:<digest>}
if [[ ! "$reference" =~ @sha256:[a-f0-9]{64}$ ]]; then
  echo "cosign-sign: expected <image>@sha256:<64 hex>, got $reference" >&2
  exit 2
fi
attempts=${COSIGN_SIGN_ATTEMPTS:-4}
backoff=${COSIGN_SIGN_BACKOFF_SECONDS:-15}

for ((attempt = 1; attempt <= attempts; attempt++)); do
  if cosign sign --yes "$reference"; then
    exit 0
  fi
  if ((attempt < attempts)); then
    echo "cosign-sign: attempt $attempt of $attempts failed for $reference; retrying" >&2
    sleep "$((attempt * backoff))"
  fi
done
echo "::error::cosign sign failed for $reference after $attempts attempts"
exit 1
