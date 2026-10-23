#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fixture_root="${repo_root}/scripts/tests/fixtures/sops-inventory"
validator="${repo_root}/scripts/validate-sops-inventory.sh"

SOPS_PRODUCTION_ROW=example-production SOPS_STAGING_ROW=example-staging \
  "${validator}" "${fixture_root}"

scratch="$(mktemp -d)"
trap 'rm -rf "${scratch}"' EXIT
cp -R "${fixture_root}/." "${scratch}/"
printf '\nNAVIGATOR_META_APP_SECRET: ENC[FIXTURE]\n' \
  >> "${scratch}/deployments/example-staging/secrets.enc.yaml"

if SOPS_PRODUCTION_ROW=example-production SOPS_STAGING_ROW=example-staging \
  "${validator}" "${scratch}" >/dev/null 2>&1; then
  echo "SOPS inventory validator accepted a production-only synthetic key in staging." >&2
  exit 1
fi

echo "SOPS inventory validator accepts the synthetic profile and rejects a misplaced key."
