#!/usr/bin/env bash
# Validate only SOPS key names. This command never calls sops, decrypts a
# value, or needs GCP credentials, so CI can prove the deployment secret
# contract without receiving KMS access.
#
# Usage: scripts/validate-sops-inventory.sh [ROOT]
#
# ROOT is the directory containing `deployments/` (default: the current
# directory). SOPS_PRODUCTION_ROW and SOPS_STAGING_ROW select the two rows.
# Their defaults name only the synthetic fixture rows used by source CI.
set -euo pipefail
export LC_ALL=C

root="${1:-.}"
production_row="${SOPS_PRODUCTION_ROW:-example-production}"
staging_row="${SOPS_STAGING_ROW:-example-staging}"
inventory="${root}/deployments/sops-inventory.yaml"
production_file="${root}/deployments/${production_row}/secrets.enc.yaml"
staging_file="${root}/deployments/${staging_row}/secrets.enc.yaml"

scratch_dir="$(mktemp -d)"
trap 'rm -rf "${scratch_dir}"' EXIT
for section in common independent production-only staging-only operator-only \
  postgres-service-only postgres-client-service-only postgres-dbos-service-only \
  forgejo-service-only forgejo-oidc-service-only; do
  : > "${scratch_dir}/${section}"
done

# The inventory deliberately has a tiny, line-oriented shape so this
# no-decrypt lane needs no YAML dependency. Secret values remain opaque SOPS
# ciphertext; only their top-level names are inspected.
section=""
while IFS= read -r line; do
  case "${line}" in
    common:) section="common" ;;
    independent:) section="independent" ;;
    production_only:) section="production-only" ;;
    staging_only:) section="staging-only" ;;
    operator_only:) section="operator-only" ;;
    postgres_service_only:) section="postgres-service-only" ;;
    postgres_client_service_only:) section="postgres-client-service-only" ;;
    postgres_dbos_service_only:) section="postgres-dbos-service-only" ;;
    forgejo_service_only:) section="forgejo-service-only" ;;
    forgejo_oidc_service_only:) section="forgejo-oidc-service-only" ;;
    [a-z]*:)
      echo "${inventory} has an unknown section: ${line}" >&2
      exit 1
      ;;
    '  '[A-Z_]*:*)
      if [ -z "${section}" ]; then
        echo "${inventory} lists a key outside any section: ${line}" >&2
        exit 1
      fi
      key="${line#  }"
      printf '%s\n' "${key%%:*}" >> "${scratch_dir}/${section}"
      ;;
  esac
done < "${inventory}"

listed="${scratch_dir}/listed"
cat "${scratch_dir}"/common "${scratch_dir}"/independent "${scratch_dir}"/production-only \
  "${scratch_dir}"/staging-only "${scratch_dir}"/operator-only \
  "${scratch_dir}"/postgres-service-only "${scratch_dir}"/postgres-client-service-only \
  "${scratch_dir}"/postgres-dbos-service-only \
  "${scratch_dir}"/forgejo-service-only "${scratch_dir}"/forgejo-oidc-service-only | sort > "${listed}"
duplicated="$(uniq -d "${listed}")"
if [ -n "${duplicated}" ]; then
  echo "${inventory} lists key(s) in more than one section:" >&2
  echo "${duplicated}" >&2
  exit 1
fi
for section in common independent production-only staging-only postgres-service-only \
  postgres-client-service-only postgres-dbos-service-only forgejo-service-only forgejo-oidc-service-only; do
  sort -u -o "${scratch_dir}/${section}" "${scratch_dir}/${section}"
done

# Every top-level value must be SOPS ciphertext. A plaintext value, or a
# block scalar that would hide one on the following lines, fails here before
# any name comparison.
reject_plaintext() {
  local file="$1"
  local plaintext
  plaintext="$(awk '/^[A-Z][A-Z0-9_]*:/ { value = $0; sub(/^[^:]*:[ \t]*/, "", value); if (value !~ /^ENC\[/) { sub(/:.*/, ""); print } }' "${file}")"
  if [ -n "${plaintext}" ]; then
    echo "${file} carries plaintext value(s); encrypt each with sops before committing:" >&2
    echo "${plaintext}" >&2
    exit 1
  fi
}

sops_keys() {
  awk '/^[A-Z][A-Z0-9_]*: ENC\[/{sub(/:.*/, ""); print}' "$1" | sort -u
}

reject_plaintext "${production_file}"
reject_plaintext "${staging_file}"

production_actual="${scratch_dir}/production-actual"
staging_actual="${scratch_dir}/staging-actual"
sops_keys "${production_file}" > "${production_actual}"
sops_keys "${staging_file}" > "${staging_actual}"

# Production-only material is checked by name before the general allowlist so
# a copy into staging is reported as the leak it is.
leaked="$(comm -12 "${staging_actual}" "${scratch_dir}/production-only")"
if [ -n "${leaked}" ]; then
  echo "staging SOPS file carries production-only key(s); never copy them to staging:" >&2
  echo "${leaked}" >&2
  exit 1
fi

cat "${scratch_dir}"/common "${scratch_dir}"/independent "${scratch_dir}"/production-only \
  | sort -u > "${scratch_dir}/production-allowed"
cat "${scratch_dir}"/common "${scratch_dir}"/independent "${scratch_dir}"/staging-only \
  | sort -u > "${scratch_dir}/staging-allowed"

for row in production staging; do
  if [ "${row}" = production ]; then
    deployment_name="${production_row}"
  else
    deployment_name="${staging_row}"
  fi
  actual="${scratch_dir}/${row}-actual"
  allowed="${scratch_dir}/${row}-allowed"
  unexpected="${scratch_dir}/${row}-unexpected"

  leaked="${scratch_dir}/${row}-service-in-web-file"
  comm -12 "${actual}" "${scratch_dir}/postgres-service-only" > "${leaked}"
  comm -12 "${actual}" "${scratch_dir}/postgres-client-service-only" >> "${leaked}"
  comm -12 "${actual}" "${scratch_dir}/postgres-dbos-service-only" >> "${leaked}"
  comm -12 "${actual}" "${scratch_dir}/forgejo-service-only" >> "${leaked}"
  comm -12 "${actual}" "${scratch_dir}/forgejo-oidc-service-only" >> "${leaked}"
  if [ -s "${leaked}" ]; then
    echo "${row} web/workflow secrets.enc.yaml contains service-only key(s); move them to the dedicated service file:" >&2
    cat "${leaked}" >&2
    exit 1
  fi

  comm -23 "${actual}" "${allowed}" > "${unexpected}"
  if [ -s "${unexpected}" ]; then
    echo "${row} SOPS file contains key(s) ${inventory} does not allow in that row:" >&2
    cat "${unexpected}" >&2
    exit 1
  fi

  service_actual="${scratch_dir}/${row}-postgres-service-actual"
  service_allowed="${scratch_dir}/${row}-postgres-service-allowed"
  # The DBOS login is optional: allowed, never required.
  sort -u "${scratch_dir}/postgres-service-only" "${scratch_dir}/postgres-client-service-only" \
    "${scratch_dir}/postgres-dbos-service-only" > "${service_allowed}"
  service_file="${root}/deployments/${deployment_name}/postgres-secrets.enc.yaml"
  : > "${service_actual}"
  if [ -f "${service_file}" ]; then
    reject_plaintext "${service_file}"
    sops_keys "${service_file}" > "${service_actual}"
  fi
  missing="${scratch_dir}/${row}-postgres-service-missing"
  comm -23 "${scratch_dir}/postgres-service-only" "${service_actual}" > "${missing}"
  # The client logins are optional as a set: all of them or none.
  client_present="${scratch_dir}/${row}-postgres-client-present"
  comm -12 "${scratch_dir}/postgres-client-service-only" "${service_actual}" > "${client_present}"
  if [ -s "${client_present}" ]; then
    comm -23 "${scratch_dir}/postgres-client-service-only" "${service_actual}" >> "${missing}"
  fi
  if [ -f "${service_file}" ] && [ ! -s "${service_actual}" ]; then
    echo "${row} PostgreSQL service file has no encrypted service keys:" >&2
    echo "${service_file}" >&2
    exit 1
  fi
  unexpected="${scratch_dir}/${row}-postgres-service-unexpected"
  comm -23 "${service_actual}" "${service_allowed}" > "${unexpected}"
  if [ -s "${unexpected}" ]; then
    echo "${row} PostgreSQL service file contains key(s) not allowed by ${inventory}:" >&2
    cat "${unexpected}" >&2
    exit 1
  fi
  if [ -s "${service_actual}" ] && [ -s "${missing}" ]; then
    echo "${row} PostgreSQL service file is incomplete; missing key(s) from ${inventory}:" >&2
    cat "${missing}" >&2
    exit 1
  fi

  service_actual="${scratch_dir}/${row}-forgejo-service-actual"
  service_allowed="${scratch_dir}/${row}-forgejo-service-allowed"
  # The OIDC client secret is optional: allowed, never required.
  sort -u "${scratch_dir}/forgejo-service-only" "${scratch_dir}/forgejo-oidc-service-only" \
    > "${service_allowed}"
  service_file="${root}/deployments/${deployment_name}/forgejo-secrets.enc.yaml"
  : > "${service_actual}"
  if [ -f "${service_file}" ]; then
    reject_plaintext "${service_file}"
    sops_keys "${service_file}" > "${service_actual}"
  fi
  missing="${scratch_dir}/${row}-forgejo-service-missing"
  comm -23 "${scratch_dir}/forgejo-service-only" "${service_actual}" > "${missing}"
  if [ -f "${service_file}" ] && [ ! -s "${service_actual}" ]; then
    echo "${row} Forgejo service file has no encrypted service keys:" >&2
    echo "${service_file}" >&2
    exit 1
  fi
  unexpected="${scratch_dir}/${row}-forgejo-service-unexpected"
  comm -23 "${service_actual}" "${service_allowed}" > "${unexpected}"
  if [ -s "${unexpected}" ]; then
    echo "${row} Forgejo service file contains key(s) not allowed by ${inventory}:" >&2
    cat "${unexpected}" >&2
    exit 1
  fi
  if [ -s "${service_actual}" ] && [ -s "${missing}" ]; then
    echo "${row} Forgejo service file is incomplete; missing key(s) from ${inventory}:" >&2
    cat "${missing}" >&2
    exit 1
  fi

done

production_common="${scratch_dir}/production-common"
staging_common="${scratch_dir}/staging-common"
comm -12 "${scratch_dir}/common" "${production_actual}" > "${production_common}"
comm -12 "${scratch_dir}/common" "${staging_actual}" > "${staging_common}"
if ! diff -u "${production_common}" "${staging_common}"; then
  echo "common SOPS keys must occur in both rows or neither" >&2
  exit 1
fi

echo "SOPS key inventory is valid; no value was decrypted."
