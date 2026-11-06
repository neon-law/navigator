#!/usr/bin/env bash
# Move images/postgres/Containerfile to the newest upstream releases within
# its PostgreSQL major: the PostgreSQL minor and its base-image digest, Apache
# AGE, and pgvector, each with a freshly verified source checksum.
#
#   scripts/postgres-image-bump.sh
#
# The version tag `<pg>-age<age>-vector<pgvector>` is rewritten wherever it
# appears (the publish workflow, manifests, docs), so a merged bump publishes
# under its new tag. `.github/workflows/postgres-image.yml` runs this weekly
# and opens a pull request, the way Dependabot does for crates. PG_MAJOR never
# moves here: a major upgrade needs a data migration.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
recipe=images/postgres/Containerfile
arg() { sed -n "s/^ARG $1=//p" "$recipe"; }
set_arg() { perl -pi -e "s/^ARG $1=.*/ARG $1=$2/" "$recipe"; }
latest() { sort -V | tail -n1; }

major=$(arg PG_MAJOR)
old_tag="$(arg PG_VERSION)-age$(arg AGE_VERSION)-vector$(arg PGVECTOR_VERSION)"

pg=$(curl -fsS "https://hub.docker.com/v2/repositories/library/postgres/tags?page_size=100&name=${major}." \
  | jq -r '.results[].name' | sed -n "s/^\(${major}\.[0-9]*\)-bookworm$/\1/p" | latest)
pg_digest=$(curl -fsS "https://hub.docker.com/v2/repositories/library/postgres/tags/${pg}-bookworm" | jq -r .digest)
[[ "$pg" =~ ^${major}\.[0-9]+$ && "$pg_digest" =~ ^sha256:[a-f0-9]{64}$ ]]

# AGE tags each release per PostgreSQL major, e.g. PG16/v1.6.0-rc0.
age=$(curl -fsS "https://api.github.com/repos/apache/age/git/matching-refs/tags/PG${major}/v" \
  | jq -r '.[].ref' | sed -n "s|^refs/tags/PG${major}/v\([0-9.]*\)-rc[0-9]*$|\1|p" | latest)
vector=$(curl -fsS "https://api.github.com/repos/pgvector/pgvector/git/matching-refs/tags/v" \
  | jq -r '.[].ref' | sed -n 's|^refs/tags/v\([0-9]*\.[0-9]*\.[0-9]*\)$|\1|p' | latest)
[[ -n "$age" && -n "$vector" ]]

set_arg PG_VERSION "$pg"
set_arg PG_DIGEST "$pg_digest"
if [[ "$age" != "$(arg AGE_VERSION)" ]]; then
  # Apache publishes the release manager's checksum beside the tarball.
  sum=$(curl -fsS "https://downloads.apache.org/age/PG${major}/${age}/apache-age-${age}-src.tar.gz.sha512" | awk '{print $1}')
  [[ "$sum" =~ ^[a-f0-9]{128}$ ]]
  set_arg AGE_VERSION "$age"
  set_arg AGE_SHA512 "$sum"
fi
if [[ "$vector" != "$(arg PGVECTOR_VERSION)" ]]; then
  sum=$(curl -fsSL "https://github.com/pgvector/pgvector/archive/refs/tags/v${vector}.tar.gz" | shasum -a 256 | awk '{print $1}')
  set_arg PGVECTOR_VERSION "$vector"
  set_arg PGVECTOR_SHA256 "$sum"
fi

new_tag="${pg}-age${age}-vector${vector}"
if [[ "$new_tag" != "$old_tag" ]]; then
  git grep -lzF "$old_tag" -- ':!server/public/reports' \
    | OLD=$old_tag NEW=$new_tag xargs -0 perl -pi -e 's/\Q$ENV{OLD}\E/$ENV{NEW}/g'
  # Prose names the components separately; keep it in step.
  old_pg=${old_tag%%-*}
  perl -pi -e "s/PostgreSQL \Q$old_pg\E/PostgreSQL $pg/g; s/AGE [0-9.]+[0-9]/AGE $age/g; s/pgvector [0-9.]+[0-9]/pgvector $vector/g" \
    images/postgres/README.md docs/self-hosting.md docs/postgres-compatibility.md
fi
echo "postgres-image-bump: $old_tag -> $new_tag (base $pg_digest)"
