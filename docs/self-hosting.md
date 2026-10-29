# Self-hosting Navigator

This guide takes a fresh clone to a running local `web`, then covers the pieces an operator assembles for a cluster of
their own: images, PostgreSQL migrations, tenancy, backups, durable workflows, and upgrades. Local startup and the
offline upgrade plan run without a Neon Law account, cloud project, SOPS key, registry login, or access token. A live
cloud deployment and the supplied Google Cloud Storage backup manifests require the operator's own services and
credentials.

Navigator is mid-migration from SurrealDB and Restate to PostgreSQL and DBOS. Sections marked **Interim** describe a
component that ships today and is scheduled to retire. The table distinguishes delivered behavior from the target.

| Concern | Today | Target |
| --- | --- | --- |
| System of record | SurrealDB (**Interim**) | PostgreSQL |
| PostgreSQL | Migrations, Firm journal, and mirror tables; opt-in | Every read and write |
| Durable workflows | Restate (**Interim**) | DBOS inside `web` |
| DBOS | Inside `web`, opt-in; `Heartbeat` and five flagged paths | Every durable job |
| Apache AGE | In the PostgreSQL image; no application reads | Graph queries over the same database |

## Prerequisites

To build and test:

- [`rustup`](https://rustup.rs). It reads the toolchain pin in `rust-toolchain.toml`.
- A C/C++ toolchain and linker: Xcode command line tools on macOS, or `build-essential` on Linux. Windows is not
  supported.
- `cmake` and `git`.

To run the local tier, also:

- Docker, running.
- `kind` v0.32.0, the version CI pins against its `kindest/node` image.
- `kubectl` and `helm` (any release with OCI registry support).

[`oss-install.md`](oss-install.md#0-prerequisites) explains each pin.

## Fresh clone to a running web

```bash
git clone https://github.com/neon-law/navigator.git
cd navigator
cargo run -p cli -- dev up --path "$PWD"
set -a; source .devx/env; set +a
cargo run -p neon
```

`dev up` creates the `navigator` KIND cluster and builds the `workflows-service` image from the checkout. It applies the
dependency overlay `k8s/overlays/kind-deps`: SurrealDB, Rauthy, SeaweedFS, Restate, ClamAV, and OpenObserve. It then
opens host port-forwards, applies the SurrealDB schema, and writes `.devx/env`. That file is the complete environment
for a host-side `web`. It carries only local, synthetic values, and `NAVIGATOR_CI_HARNESS=1` lets the dev profile boot
with fake email and signature providers. The website listens on `PORT` from `.devx/env` (default `3001`).

No `.env` is needed. When one exists it loads before `.devx/env` and wins on any shared key; copy
[`.env.example`](../.env.example) to start one with your own provider credentials.

The dev profile seeds the public firm facts and three synthetic sample matters. `dev up` also stages each sample
matter's application bundle from its public, read-only `neon-law/sample-*` snapshot. When one is unreachable, for
example offline, `dev up` prints which matters keep their previously staged source and continues, and `web` boots
without that bundle.

Check the running web from another terminal after sourcing `.devx/env`:

```bash
curl --fail "http://127.0.0.1:${PORT}/app/health"
```

The response begins with `ok`. Stop the host `web` process with Ctrl-C, then tear the tier down with:

```bash
cargo run -p cli -- dev down --path "$PWD"
```

### Failure messages

| Message | Fix |
| --- | --- |
| `required tool not on PATH: <tool>` | Install the named prerequisite above. |
| `<dir> is not a Navigator checkout (it needs Cargo.toml and k8s/ at its root)` | Pass the repository root. |
| `could not find workspace root containing Cargo.toml and k8s/` | Run from inside the checkout, or pass `--path`. |

Two tests hold this path. `cli/tests/help.rs` runs `dev up --path` with an empty environment and asserts it stops only
at a missing public tool, never at a cloud credential. `cli/src/devx/mod.rs` renders `.devx/env` with every private
credential variable stripped and asserts it passes the web boot invariants and names no private registry, bucket, or
project.

For parallel worktrees, the per-worktree lane is `navigator dev worktree-env up`; see
[`env-driven-devx.md`](env-driven-devx.md).

## Images

Each release builds these images from `images/`, tagged with the release version:

| Image | Containerfile | Runs |
| --- | --- | --- |
| `neon-server` | `images/Containerfile.neon` | `web`, and `/app/navigator-migrate` |
| `navigator-workflows-service` | `images/Containerfile.workflows-service` | the Restate worker (**Interim**) |
| `navigator-*-trigger`, `navigator-general-nag` | `images/Containerfile.trigger` | scheduled workflow triggers |

The release workflow mirrors every digest to `ghcr.io/<owner>/navigator/<image>:<tag>` and signs the mirror with cosign.
Pull those anonymously, or build your own:

```bash
docker build -f images/Containerfile.neon -t registry.example/navigator/neon-server:<tag> .
```

Point the CLI's image lookups at a public or self-run registry with `NAVIGATOR_IMAGE_REGISTRY`:

```bash
export NAVIGATOR_IMAGE_REGISTRY=ghcr.io/<owner>/navigator
cargo run -p cli -- dev deploy
```

`dev deploy` runs the full in-cluster stack from published images. `dev up` never contacts a registry for Navigator's
own images.

### PostgreSQL and Apache AGE image

`ghcr.io/neon-law/neon-law-postgres:16.15-age1.6.0-vector0.8.2` is PostgreSQL 16.15 with Apache AGE 1.6.0 and pgvector
0.8.2, built from `images/postgres/Containerfile`. The manifests under `k8s/postgres/` pin it by digest.
[`postgres-compatibility.md`](postgres-compatibility.md) lists the tested matrix.

## PostgreSQL and SQL migrations

Migrations live in `store/postgres/migrations` as timestamped SQLx files. `navigator-migrate` is the one runner. It
compiles the migrations in, ships in `neon-server` at `/app/navigator-migrate`, and reads the discrete
`NAVIGATOR_POSTGRES_*` connection values (`HOST`, `PORT`, `DATABASE`, `USER`, `PASSWORD`, `SSLMODE`, `ROOT_CERT`).

```bash
cargo run -p store --bin navigator-migrate                          # every pending migration
cargo run -p store --bin navigator-migrate -- --through <version>   # pending migrations up to <version>
```

Locally, `cargo run -p cli -- dev forgejo up` adds PostgreSQL, its roles, and Forgejo to the tier, forwarded on
`NAVIGATOR_KIND_POSTGRES_PORT` (default `15432`). In a cluster, run the Job in `k8s/postgres/migrate/` with a migration
role, and start services only after it completes. `web` connects to PostgreSQL only when
`NAVIGATOR_POSTGRES_ENABLED=true`.

[`postgres-tooling.md`](postgres-tooling.md) covers adoption of a `schema.sql` install and staged migrations;
[`postgres-kubernetes.md`](postgres-kubernetes.md) covers roles, TLS, and storage.

## Tenancy: Firm, Brand, Project

A Firm owns Projects. `firm_brand` records which Brands a Firm may use, and a Project's Brand must be one its owning
Firm uses. Moving a Project names the target Firm and Brand together. Memberships are `person_firm_role` and
`person_project_role`.

Every table's scope is declared in `store/postgres/ownership.json` and enforced in queries, not by row-level security
alone. Look up each term with `cargo run -p cli -- glossary show "Firm"`. [`postgres-tenancy.md`](postgres-tenancy.md)
has the per-table matrix.

A self-hosted deployment brands itself through a brand bundle, not by editing views. See
[`oss-install.md`](oss-install.md#brand-bundle).

## Object storage

SeaweedFS 4.48 (Apache-2.0) supplies S3 storage in KIND and native macOS development. The Homebrew formula is
`seaweedfs`; its executable is `weed`. The application uses the existing S3 adapter with region `us-east-1`. KIND
exposes only S3 on port 8333 inside the cluster; the host forward remains 30900 by default. Master, filer, and volume
listeners stay on loopback. The single-node development topology is not a production HA deployment.

The CLI generates independent credentials for documents, assets, applications, exports, archives, telemetry, LFS, and
archived Projects. Each identity can access only its own bucket. KIND stores the keys and startup `s3.json` together in
`navigator-seaweedfs-s3`; native development stores credentials in files readable only by their owner and persists
identities through the filer. Provisioning preserves keys on repeated runs and after restart.

Local object storage is disposable fixture infrastructure. Recreate the development environment and seed fresh synthetic
data when changing its storage engine. Production continues to use its configured storage provider.

## Backups

- **PostgreSQL.** `k8s/postgres/backup/` streams WAL continuously and takes a daily base backup. Both write to a Google
  Cloud Storage bucket the operator names in the `navigator-postgres-backup-target` ConfigMap.
  `k8s/postgres/restore-drill/` restores into scratch storage and reports row counts and AGE graph counts. See
  [`postgres-kubernetes.md`](postgres-kubernetes.md#backups-and-point-in-time-recovery).
- **SurrealDB (Interim).** `navigator ops surreal-archive export` writes a best-effort recovery archive by default, and
  `navigator ops surreal-archive restore-drill --key <key>` checks it against its export receipt when available. This
  verifies recovery only; strict consistency requires an explicitly quiesced-writer export. See
  [`surreal-archives.md`](surreal-archives.md).
- **Projects.** Each Project has verified recovery archives under `/app/api/projects/{id}/recovery-archives`. See
  [`project-recovery-archives.md`](project-recovery-archives.md).

## DBOS

DBOS runs inside `web` when `NAVIGATOR_DBOS_ENABLED=true`, keeping its system schema in PostgreSQL. `Heartbeat` runs on
DBOS. `GeneralNag`, `DriDigest`, `ProjectRecovery`, `RetentionPurge`, and `SourceImport` each have a DBOS path behind
its own flag. Do not rewrite DBOS's system tables by hand. [`dbos-durability.md`](dbos-durability.md) has the flags and
the effect-key rule.

## Restate (Interim)

Restate runs every other durable workflow. `dev up` installs the pinned Restate Operator with Helm, and
`k8s/staging/restate.yaml` declares the cluster. `workflows-service` is the registered worker.
[`durable-workflows.md`](durable-workflows.md) lists what it owns.

## Apache AGE

AGE ships in the PostgreSQL image. No application code queries it yet, and no application role is granted `ag_catalog`.
The restore drill and the compatibility tests verify graph data survives a restore. Treat AGE's internal IDs as the
extension's own, not as application keys.

## Forgejo

The opt-in `k8s/forgejo/` StatefulSet runs Forgejo beside Navigator, with its own `forgejo` database and login on the
same PostgreSQL server. [`forgejo-deployment.md`](forgejo-deployment.md) is the operator guide. It covers:

- **Sign-in.** Forgejo uses Google OAuth with its own client and callback, without a separate local password or factor,
  and keeps registration closed until the email-domain allowlist is synced. KIND uses the synthetic Rauthy OIDC fixture.
- **The Project gate.** `navigator project scaffold` writes `.forgejo/workflows/gate.yml` for a Forgejo-backed Project.
- **Git bundle archive and restore.** `navigator ops forgejo archive` and `navigator ops forgejo restore` prove one Git
  bundle round trip per invocation.

Locally, `cargo run -p cli -- dev forgejo up` starts it; see [`forgejo-kind.md`](forgejo-kind.md).

The Iceberg export covers the databases only; it does not cover Git repositories, LFS objects, or attachments. Those
live on Forgejo's retained `/var/lib/gitea` claim and need their own backup, restored to a matching recovery point. See
[`iceberg-archive.md`](iceberg-archive.md) for the export.

## Kubernetes layers

| Path | Holds |
| --- | --- |
| `k8s/base/` | The `navigator` namespace, shared by every overlay |
| `k8s/base/web/` | The `navigator-web` Deployment, its Secret, and its repository volume |
| `k8s/staging/` | A complete provider-neutral runtime: base, web, Rauthy, Restate, OpenObserve, worker, ClamAV |
| `k8s/postgres/` | PostgreSQL, roles, migrations, mirror reconcile, backups, and the restore drill |
| `k8s/overlays/kind/` | The full local stack: staging plus SeaweedFS, SurrealDB, and the trigger CronJobs |
| `k8s/overlays/kind-deps/` | The `dev up` stack: the same, minus `web`, which runs on the host |

A cluster of your own starts from `k8s/staging/` plus `k8s/postgres/`, with a private overlay for object storage,
ingress, and secrets. [`multi-cloud.md`](multi-cloud.md) and [`oss-install.md`](oss-install.md) cover provider overlays.

## Deployment configuration

`navigator ops ship` reads an operator-supplied root through `--deployments-dir`, then `NAVIGATOR_DEPLOYMENTS_DIR`, then
the source checkout. The root contains `deployments/<name>/config.toml` and `.sops.yaml`; pass the containing directory,
not `deployments/` itself. Keep live rows and encrypted secrets in a private operator checkout. The source tree carries
only the synthetic `example-deployment` row.

From the source checkout, render an offline upgrade plan using that example:

```bash
cargo run -p cli -- ops ship --deployment example-deployment --deployments-dir "$PWD" \
  --tag 26.10.19 --dry-run
```

This validates local configuration and renders manifests without cloud tools or credentials. The synthetic encrypted
markers are planning fixtures, so this example supports only `--dry-run`. A successful plan verifies neither live access
nor images, decrypted secrets, IAM, assets, Restate registration, or health endpoints. For an authenticated rollout,
supply a private deployment root and follow [`manual-roll.md`](manual-roll.md).

## Upgrades

Releases are tagged `YY.M.D` (for example `26.10.19`); a `-hotfix.N` suffix is a hotfix. Upgrade in this order, with
every image at the same tag:

1. PostgreSQL server manifest, when the image digest changes.
2. The roles Job in `k8s/postgres/access/`.
3. The migration Job in `k8s/postgres/migrate/`, pinned to the new `neon-server` digest. Wait for it to complete, then
   delete it so the next release can recreate it.
4. `web`, `workflows-service`, and the trigger CronJobs.

Roll back the application by redeploying the previous tag. Migrations are forward-only, so read a release's migrations
before rolling back across them. Some migrations need a staged run; [`postgres-tooling.md`](postgres-tooling.md) names
them. Upgrade a staging cluster before production.

## Not yet covered

These are open and land in later changes:

- PostgreSQL becomes the system of record for every read, and SurrealDB retires.
- DBOS takes the remaining durable workflows, and Restate and `workflows-service` retire.
- `dev up` includes PostgreSQL without the separate `dev forgejo up` step.
- PostgreSQL backups write to any S3-compatible bucket, not only Google Cloud Storage.
- The sample matters' Project repositories become public, so a fresh clone stages their applications too.
