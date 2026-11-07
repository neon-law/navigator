# PostgreSQL image

`ghcr.io/neon-law/navigator/postgres` is PostgreSQL 16.15 (Debian Bookworm) with Apache AGE 1.6.0, pgvector 0.8.7, and
PostGIS 3.6.4. This directory is its source and its build context: the `Containerfile`, the Apache-2.0 `LICENSE` and
`NOTICE` copied into the image, and the smoke tests under `tests/`. The directory has its own license; the repository
root's must not replace it. It replaces `ghcr.io/neon-law/neon-law-postgres` and the `neon-law/neon-law-postgres`
repository.

The recipe's `ARG` lines pin the PostgreSQL major and minor, the base image digest, and the AGE SHA-512 and pgvector
SHA-256 source checksums. PostgreSQL supplies citext, pg_trgm and full-text search. DBOS is an application dependency
and is absent from the image. AGE needs `LOAD 'age'` and `SET search_path = ag_catalog, public` on each connection; no
preload is assumed.

The recipe pins the matching PGDG Bookworm PostGIS binary and SQL script packages for PostgreSQL 16. Their package
copyright files identify the upstream licenses. The image includes the matching PGDG source package, with the Debian
packaging and patches, under `/usr/share/source/postgis`. Applications that need spatial types require an ordered
migration to enable `postgis` in the target database after the image digest has been updated. The image provides the
extension files for both fresh and retained databases.

## Build and test locally

From the repository root:

```sh
docker build -f images/postgres/Containerfile -t navigator-postgres:local images/postgres
cd images/postgres && bash tests/run.sh navigator-postgres:local
```

`tests/run.sh` needs Docker, Cargo with Rust 1.98, curl, and a free port 8080. It creates and removes only its randomly
named container and volume. It verifies extension loading, GIN, HNSW, and spatial GiST indexes, and WGS84 geometry and
GeoJSON round trips. It also checks restart persistence, a spatial database restore, and DBOS crash recovery. The
`tests/dbos` application is a standalone Cargo project outside Navigator's workspace. Pass a published
`ghcr.io/neon-law/navigator/postgres@sha256:<digest>` to test a released image instead.

## Weekly publication

[`postgres-image.yml`](../../.github/workflows/postgres-image.yml) publishes the image:

1. **When.** Every Monday at 09:17 UTC, on any merge that changes this directory, and on manual dispatch. A pull
   request builds and tests without publishing.
2. **Bump.** The weekly run also runs [`scripts/postgres-image-bump.sh`](../../scripts/postgres-image-bump.sh), which
   moves the `ARG` pins to the newest PostgreSQL minor and base digest, Apache AGE release, and pgvector release for the
   pinned major, and opens a pull request as the automerge App. Merging it publishes the bumped image. Dependabot keeps
   the DBOS pin in `tests/dbos` in step with the workspace. A PostgreSQL major upgrade migrates data, so `PG_MAJOR`
   moves only by hand.
3. **Build.** The weekly run rebuilds without cache. The final stage holds the PostgreSQL packages and applies Debian
   updates, so each week's image carries current security fixes while the server stays at the pinned minor.
4. **Test.** AMD64 and ARM64 candidates are built and tested natively with `tests/run.sh`.
5. **Publish.** The tested candidates are promoted together without rebuilding and signed with cosign. Tags are
   `16.15-age1.6.0-vector0.8.7` (moving), `16.15-age1.6.0-vector0.8.7-YYYYMMDD`, and
   `16.15-age1.6.0-vector0.8.7-<commit>`. Consumers pin the digest, never a tag.
6. **Dogfood.** [`scripts/postgres-image-pin.sh`](../../scripts/postgres-image-pin.sh) moves every Navigator pin to
   the new digest, and the workflow opens a pull request as the automerge App. That pull request runs [PostgreSQL
   compatibility](../../.github/workflows/postgres-compatibility.yml) and [PVC
   persistence](../../.github/workflows/postgres-persistence.yml) against the published digest. After merge, staging
   takes it with the next release, following the [staging-first
   rollout](../../docs/postgres-kubernetes.md#postgresql-image-updates).

The package must be public: the compatibility gate and the clusters pull it anonymously. A new GHCR package starts
private, so its visibility is set once in the package settings after its first publish.

Verify a published image's signature:

```sh
cosign verify ghcr.io/neon-law/navigator/postgres@sha256:<digest> \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  --certificate-identity-regexp '^https://github.com/neon-law/navigator/\.github/workflows/postgres-image\.yml@'
```

## Pins

Every consumer pins the same digest: the server, access, backup, restore-drill, Forgejo database, and disposable spike
kustomizations; the expected digest in `postgres-persistence.yml`; the CI PostgreSQL service in `ci.yml`; and
`cli/tests/staging_manifests.rs`, which fails when they drift. Move them by hand with the same script:

```sh
scripts/postgres-image-pin.sh ghcr.io/neon-law/navigator/postgres@sha256:<digest>
```

## Run it in KIND

Local KIND and CI consume the pinned digest from `k8s/overlays/postgres-spike/kustomization.yaml`:

```sh
NAVIGATOR_KIND_DEPS_OVERLAY=k8s/overlays/kind-postgres-spike \
  navigator dev worktree-env up --runtime kind --path "$PWD"
set -a; source .devx/env; set +a
kubectl --kubeconfig .devx/kubeconfig --context "kind-$NAVIGATOR_KIND_CLUSTER" \
  -n navigator rollout status statefulset/postgres-spike --timeout=180s
kubectl --kubeconfig .devx/kubeconfig --context "kind-$NAVIGATOR_KIND_CLUSTER" \
  -n navigator port-forward service/postgres-spike :5432
```

Read the assigned loopback port from the port-forward output. This is a disposable synthetic database alongside the
existing Surreal and Restate dependencies. At handoff, remove only the task-owned tier:

```sh
navigator dev worktree-env down --path "$PWD"
```

Keep the existing PostgreSQL major version, retained PVC, and installed extension versions compatible with each new
image. A major-version upgrade needs a separate migration procedure. Application PostgreSQL opt-in is independent of an
image update.
