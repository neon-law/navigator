---
name: postgres
description: >-
  Maintain PostgreSQL storage, migrations, AGE, pgvector, DBOS, image pins, and deployments. Use before new storage
  design.
---

# PostgreSQL

Ground in the code, its tests, and the [glossary](../../../docs/glossary) first.

Use PostgreSQL relational models and ordered SQLx migrations for new storage work. Prefer supported PostgreSQL writes
through the domain accessor. [Storage models and writes](../../../docs/storage-models.md) records authority selection
for People and the AGE graph, PostgreSQL-native domains, and cutover limits. A mirror alone does not transfer write
authority. The schema and parity inventory live under `store/postgres/`.

Apply migrations through the target's authority boundary before application boot. Fresh native creation and its explicit
bound upgrade own their migration step; do not run an independent migrator around that upgrade intent. Captured
deployments use `navigator-migrate` ([tooling](../../../docs/postgres-tooling.md)). Request handlers and the runtime
importer perform no DDL. [SurrealDB compatibility](../../../docs/surreal-compatibility.md) holds the remaining
source-engine contracts; [surreal-migrations](../surreal-migrations/SKILL.md) governs changes to those live paths.

## The image

- **Published image:** `ghcr.io/neon-law/navigator/postgres`, built here weekly, upstream versions bumped weekly,
  and pinned by digest.
- **Digest pins (keep identical):** [`k8s/postgres/kustomization.yaml`](../../../k8s/postgres/kustomization.yaml),
  its `access/`, `backup/`, and `restore-drill/` kustomizations,
  [`k8s/forgejo/database/kustomization.yaml`](../../../k8s/forgejo/database/kustomization.yaml), and
  [`k8s/overlays/postgres-spike/kustomization.yaml`](../../../k8s/overlays/postgres-spike/kustomization.yaml).
  `cli/tests/staging_manifests.rs` fails when they drift.
- **Recipe, tests, and build context:** [`images/postgres/`](../../../images/postgres/README.md).
- **Weekly publish:** [`postgres-image.yml`](../../../.github/workflows/postgres-image.yml) builds, tests, signs, and
  publishes, then opens a pull request moving every pin with
  [`scripts/postgres-image-pin.sh`](../../../scripts/postgres-image-pin.sh).
- **CI gates:** [workspace CI](../../../.github/workflows/ci.yml), [native KIND
  E2E](../../../.github/workflows/quickstart.yml), and [PVC persistence and
  recovery](../../../.github/workflows/postgres-persistence.yml).
- **Retained StatefulSet (GKE):** [`docs/postgres-kubernetes.md`](../../../docs/postgres-kubernetes.md).
- **Decision and evidence:**
  [`postgres-compatibility-spike.html`](../../../server/public/reports/postgres-compatibility-spike.html).

The image is `postgres:16.15-bookworm` (pinned by digest) plus two extensions built from checksummed source:

- **Apache AGE 1.6.0** for graph queries, which replace SurrealDB's record links and graph edges.
- **pgvector 0.8.2** for embeddings.

Stock PostgreSQL supplies `citext`, `pg_trgm`, and full-text search. **DBOS** is the durable-execution target. It is an
application dependency, not part of the image.

## Rules

- **Pin by digest, everywhere at once.** `k8s/postgres/` (the retained `navigator-postgres` StatefulSet),
  `k8s/overlays/postgres-spike/` (the disposable `postgres-spike`), and the other pins above move together. Never use a
  tag or `latest` in a shared environment.
- **Change the recipe here.** Edit `images/postgres/` in a pull request; `postgres-image.yml` smoke-tests both
  architectures on it and publishes on merge. Move pins only with `scripts/postgres-image-pin.sh`, never by hand.
- **No committed credentials.** The spike's `POSTGRES_PASSWORD: synthetic` is a disposable local/CI fixture. The
  retained StatefulSet reads `navigator-postgres-auth` and `navigator-postgres-tls`, which a Deployment Operator
  provisions through the environment's secret path. Clients use `sslmode=verify-full`, carry the
  `navigator.neonlaw.org/postgres-client: "true"` label, and never share the bootstrap superuser.
- **Never delete the PVC or namespace in a rollout.** GKE storage is `Retain`. Deleting `data-navigator-postgres-0`
  releases the disk, and getting it back needs a recovery procedure.
- **No new SurrealDB-only design.** A new table should be relational: nouns become tables, named from the glossary. Use
  AGE only where you need a graph traversal, not as a stand-in for SurrealDB's document model.

## Build and run locally

Build the recipe from the repository root:

```bash
docker build -f images/postgres/Containerfile -t navigator-postgres:local images/postgres
```

The normal KIND lifecycle provisions the pinned retained PostgreSQL image with TLS and application roles:

```bash
cargo run -p hudson -- dev worktree-env up --runtime kind --path "$PWD"
set -a; source .devx/env; set +a
```

Use the application-role connection in `.devx/env`; it verifies the loopback server against the task-owned CA. A fresh
KIND tier creates a unique PostgreSQL database and records its [Database
Origin](../../../docs/glossary/database-origin.md) before seeding through admitted accessors. It cohosts DBOS in `web`
without creating Source or Restate services. Repeated startup verifies the same origin; unknown or partial authority
requires explicit recovery, never synthetic captured receipts or a replacement database.

Existing legacy KIND descriptors and the explicit host-process `--runtime native` topology retain their compatibility
contracts. Captured bootstrap holds writers, verifies a real export, and enrolls scopes before seeding. The native
deploy browser gate provisions through the ordinary quickstart CLI, then requires exact-image nonroot, admission,
persisted DBOS workflow replay across host/image restart and hydrated-browser acceptance without Source or Restate.
Focused unit fixtures are separate from either application topology and do not certify those complete gates.

See [kind-local-dev](../kind-local-dev/SKILL.md) for profile and retry boundaries. Tear down only your own tier with
`cargo run -p hudson -- dev worktree-env down --path "$PWD"`. Initialization or passing fixture checks do not certify
production cutover, physical recovery, or provider-effect replay.

The standalone persistence and restore drill remains in `k8s/overlays/kind-postgres-persistence`;
`postgres-persistence.yml` shows the synthetic Secrets it needs.

## What is still open

Runtime roles, AGE and DBOS grants, mandatory application admission, migration Jobs, WAL archiving, base backups, and
restore drills are defined in [`docs/postgres-kubernetes.md`](../../../docs/postgres-kubernetes.md). Captured Source
enrollment follows [`docs/postgres-tooling.md`](../../../docs/postgres-tooling.md); production transfer and recovery
acceptance require actual operator evidence. Failover and major-version upgrades require their own operational
decisions. Do not choose them in passing: coordinate with the user or
[author-linear-issue](../author-linear-issue/SKILL.md).
