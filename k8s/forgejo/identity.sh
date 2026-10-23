#!/bin/sh
# Migrate Forgejo's schema, then converge its one OpenID Connect sign-in
# source to the installation's identity provider.
#
# The issuer comes from the optional `navigator-forgejo-oidc` ConfigMap
# (`name`, `discovery_url`), which each environment's overlay commits; the
# client comes from the environment-owned `navigator-forgejo-oidc` Secret
# (`client_id`, `client_secret`). Without both, no external sign-in exists.
# The source is matched by name, so a rerun updates it in place and its users
# keep their issuer + subject link.
#
# Runs as the second init container of `navigator-forgejo`, after admission.sh
# has rendered the effective app.ini.
set -eu

oidc=${FORGEJO_OIDC_DIR:-/etc/forgejo-oidc}
oidc_secret=${FORGEJO_OIDC_SECRET_DIR:-/etc/forgejo-oidc-secret}

fj() {
  /usr/local/bin/forgejo --config /etc/forgejo/app.ini --work-path /var/lib/gitea "$@"
}

fj migrate

# Forgejo makes the first account it creates a site administrator. Hold that
# seat with a local administrator nobody can sign in as (password sign-in is
# off and its random password is discarded), so an account that registers
# through OIDC is never promoted. Operators administer through this CLI.
#
# Forgejo checks this address against EMAIL_DOMAIN_ALLOWLIST like any other, so
# with an allowlist it takes the first allowed domain; `forgejo.invalid` is
# refused as `e-mail invalid` wherever admission opened registration.
admin_domain=$(awk -F= '
  /^\[/ { section = $0; next }
  section == "[service]" && $1 ~ /^EMAIL_DOMAIN_ALLOWLIST[ \t]*$/ {
    split($2, domains, ","); gsub(/[ \t]/, "", domains[1]); print domains[1]; exit
  }' /etc/forgejo/app.ini)
if [ -z "$(fj admin user list --admin | awk 'NR > 1')" ]; then
  fj admin user create --username navigator-admin --email "navigator-admin@${admin_domain:-forgejo.invalid}" \
    --admin --random-password --random-password-length 64 >/dev/null
  echo "navigator-forgejo-identity: reserved the first administrator seat"
fi

if [ ! -s "$oidc/discovery_url" ] || [ ! -s "$oidc_secret/client_id" ] ||
  [ ! -s "$oidc_secret/client_secret" ]; then
  echo "navigator-forgejo-identity: no OIDC source configured; external sign-in stays off"
  exit 0
fi

name=$(cat "$oidc/name")
discovery_url=$(cat "$oidc/discovery_url")
client_id=$(cat "$oidc_secret/client_id")
client_secret=$(cat "$oidc_secret/client_secret")

source_id() {
  fj admin auth list --vertical-bars | awk -F'|' -v want="$name" '
    NR > 1 {
      gsub(/^[ \t]+|[ \t]+$/, "", $1)
      gsub(/^[ \t]+|[ \t]+$/, "", $2)
      if ($2 == want) print $1
    }'
}

# Only a verified address may sign in. Forgejo compares the claim through
# Go's `fmt.Sprintf("%s", value)`, so the boolean `true` that Rauthy and
# Google issue reads as `%!s(bool=true)`; a string "true", a false, or a
# missing claim is refused. The KIND smoke proves both outcomes, so a Forgejo
# change to that formatting fails closed and is caught there.
set -- --name "$name" --provider openidConnect --key "$client_id" --secret "$client_secret" \
  --auto-discover-url "$discovery_url" --scopes email --scopes profile \
  --required-claim-name email_verified --required-claim-value '%!s(bool=true)'

# Adding or updating the source fetches the discovery document, so an issuer
# that is still starting is retried. The source is looked up by name on every
# attempt, so a retry never adds a second one.
attempt=1
while :; do
  id=$(source_id)
  if [ -z "$id" ]; then
    fj admin auth add-oauth "$@" && break
  else
    fj admin auth update-oauth --id "$id" "$@" && break
  fi
  if [ "$attempt" -ge 60 ]; then
    echo "navigator-forgejo-identity: the issuer at $discovery_url never answered" >&2
    exit 1
  fi
  attempt=$((attempt + 1))
  sleep 2
done
echo "navigator-forgejo-identity: OIDC source $name converged"
