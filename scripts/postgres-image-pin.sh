#!/usr/bin/env bash
# Move every pin of the PostgreSQL image to one new reference.
#
#   scripts/postgres-image-pin.sh ghcr.io/neon-law/neon-law-postgres@sha256:<digest>
#
# The current pin is read from k8s/postgres/kustomization.yaml, the server's
# own kustomization, and every tracked file naming that repository or digest
# is rewritten: the kustomizations, the CI and persistence workflows, the
# manifest test, and the compatibility doc. `.github/workflows/postgres-image.yml`
# runs this after each weekly publish so Navigator consumes the image it ships.
set -euo pipefail

reference=${1:?usage: postgres-image-pin.sh <registry>/neon-law-postgres@sha256:<digest>}
if [[ ! "$reference" =~ ^([a-z0-9.-]+/[a-z0-9._/-]+/neon-law-postgres)@(sha256:[a-f0-9]{64})$ ]]; then
  echo "postgres-image-pin: expected <registry>/neon-law-postgres@sha256:<64 hex>, got $reference" >&2
  exit 2
fi
new_name=${BASH_REMATCH[1]}
new_digest=${BASH_REMATCH[2]}

cd "$(git rev-parse --show-toplevel)"
server=k8s/postgres/kustomization.yaml
old_name=$(awk '/newName:/ && /neon-law-postgres/ {print $2; exit}' "$server")
old_digest=$(awk '/digest: sha256:/ {print $2; exit}' "$server")
if [[ -z "$old_name" || -z "$old_digest" ]]; then
  echo "postgres-image-pin: $server carries no neon-law-postgres newName and digest" >&2
  exit 1
fi
if [[ "$old_name@$old_digest" == "$new_name@$new_digest" ]]; then
  echo "postgres-image-pin: already pinned to $reference"
  exit 0
fi

git grep -lzF -e "$old_digest" -e "newName: $old_name" -- ':!server/public/reports' \
  | OLD_NAME=$old_name OLD_DIGEST=$old_digest NEW_NAME=$new_name NEW_DIGEST=$new_digest \
    xargs -0 perl -pi -e '
      s/\Q$ENV{OLD_NAME}\E\@\Q$ENV{OLD_DIGEST}\E/$ENV{NEW_NAME}\@$ENV{NEW_DIGEST}/g;
      s/newName: \Q$ENV{OLD_NAME}\E$/newName: $ENV{NEW_NAME}/;
      s/\Q$ENV{OLD_DIGEST}\E/$ENV{NEW_DIGEST}/g;
    '

if git grep -qF "$old_digest" -- ':!server/public/reports'; then
  echo "postgres-image-pin: $old_digest is still pinned somewhere:" >&2
  git grep -nF "$old_digest" -- ':!server/public/reports' >&2
  exit 1
fi
echo "postgres-image-pin: $old_name@$old_digest -> $reference"
