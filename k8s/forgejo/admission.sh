#!/bin/sh
# Render Forgejo's effective app.ini from the deployment's base configuration
# and the email-domain allowlist.
#
# The allowlist is the `email_domain_allowlist` key of the optional
# `navigator-forgejo-admission` ConfigMap: comma-separated DNS names, exact
# matches only. A missing, empty, or malformed allowlist keeps every
# registration path closed. Forgejo itself treats an empty
# EMAIL_DOMAIN_ALLOWLIST as "any domain", so this script never opens
# registration without a valid, non-empty list.
#
# Runs as the first init container of `navigator-forgejo`; a change to the
# ConfigMap takes effect at the next pod start.
set -eu

base=${FORGEJO_BASE_CONFIG_DIR:-/etc/forgejo-base}
admission=${FORGEJO_ADMISSION_DIR:-/etc/forgejo-admission}
out=${FORGEJO_CONFIG_DIR:-/etc/forgejo}

allowlist=""
if [ -f "$admission/email_domain_allowlist" ]; then
  # Whitespace may surround entries; whitespace inside one makes it invalid.
  allowlist=$(tr '\t\r\n' '   ' < "$admission/email_domain_allowlist" |
    sed -e 's/^ *//' -e 's/ *$//' -e 's/ *, */,/g' | tr 'A-Z' 'a-z')
fi

label='[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?'
domain="$label(\\.$label)+"
if [ -n "$allowlist" ] && ! printf '%s\n' "$allowlist" | grep -Eqx "$domain(,$domain)*"; then
  echo "navigator-forgejo-admission: email_domain_allowlist is not a comma-separated list of DNS names; registration stays closed" >&2
  allowlist=""
fi

if [ -n "$allowlist" ]; then open=true; else open=false; fi

awk -v allow="$allowlist" -v open="$open" '
  /^\[/ { section = $0 }
  section == "[service]" && /^DISABLE_REGISTRATION[ =]/ {
    print "DISABLE_REGISTRATION = " (open == "true" ? "false" : "true"); next
  }
  section == "[service]" && /^EMAIL_DOMAIN_ALLOWLIST[ =]/ { print "EMAIL_DOMAIN_ALLOWLIST = " allow; next }
  section == "[oauth2_client]" && /^ENABLE_AUTO_REGISTRATION[ =]/ { print "ENABLE_AUTO_REGISTRATION = " open; next }
  { print }
' "$base/app.ini" > "$out/app.ini"

for key in DISABLE_REGISTRATION EMAIL_DOMAIN_ALLOWLIST ENABLE_AUTO_REGISTRATION; do
  if [ "$(grep -c "^$key = " "$out/app.ini")" != 1 ]; then
    echo "navigator-forgejo-admission: base app.ini must define $key exactly once" >&2
    exit 1
  fi
done

if [ -f "$base/oauth2_jwt_secret" ]; then
  cp "$base/oauth2_jwt_secret" "$out/oauth2_jwt_secret"
  chmod 0400 "$out/oauth2_jwt_secret"
fi

if [ "$open" = true ]; then
  echo "navigator-forgejo-admission: external registration open to $allowlist"
else
  echo "navigator-forgejo-admission: registration closed"
fi
