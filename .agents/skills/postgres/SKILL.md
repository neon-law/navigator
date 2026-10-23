---
name: postgres
description: >
  PostgreSQL is replacing SurrealDB as Navigator's store. Use this skill for the image we run
  (`ghcr.io/neon-law-source-code/neon-law-postgres`: PostgreSQL 16.15 + Apache AGE 1.6.0 + pgvector 0.8.2), its digest
  pin, the retained GKE StatefulSet, the local KIND overlays, and the CI gates. Trigger when the user mentions Postgres,
  the Surreal swap, AGE, pgvector, DBOS, or `images/postgres/`; when a change edits `k8s/postgres/`,
  `k8s/overlays/*postgres*`, or `examples/deploy/k8s/gke/postgres/`; or before designing a new table or store client.
---

# PostgreSQL

Before beginning, always ground in the code, its tests, and the [glossary](../../../docs/glossary).

We are swapping SurrealDB for PostgreSQL. The image and infrastructure foundation are tested. A partial experimental SQL
schema and parity inventory exist under `store/postgres/`; they are not a complete application schema. The Firm journal
uses SQLx and PostgreSQL-only writes, with a temporary boot-time import of Surreal Journal Entries. Missing insertion
timestamps require read-only triage; they are never invented. Other accessors still use SurrealDB. Run
`navigator-migrate` before a PostgreSQL-configured boot ([tooling](../../../docs/postgres-tooling.md)); the runtime
importer performs no DDL. No deployment's `web` reads PostgreSQL until its `config.toml` opts in. Until then,
[surreal-migrations](../surreal-migrations/SKILL.md) governs every live schema change.

## The image

- **Published image:** `ghcr.io/neon-law-source-code/neon-law-postgres`, pinned by digest.
- **Digest pins (keep identical):** [`k8s/postgres/kustomization.yaml`](../../../k8s/postgres/kustomization.yaml),
  its `access/`, `backup/`, and `restore-drill/` kustomizations,
  [`k8s/forgejo/database/kustomization.yaml`](../../../k8s/forgejo/database/kustomization.yaml), and
  [`k8s/overlays/postgres-spike/kustomization.yaml`](../../../k8s/overlays/postgres-spike/kustomization.yaml).
  `cli/tests/staging_manifests.rs` fails when they drift.
- **Recipe (build context):** [`images/postgres/`](../../../images/postgres/README.md).
- **Upstream source:** `neon-law-source-code/neon-law-postgres` at commit `155a38bb`.
- **CI gates:** [`postgres-compatibility.yml`](../../../.github/workflows/postgres-compatibility.yml) and
  [`postgres-persistence.yml`](../../../.github/workflows/postgres-persistence.yml).
- **Retained StatefulSet (GKE):** [`docs/postgres-kubernetes.md`](../../../docs/postgres-kubernetes.md).
- **Decision and evidence:**
  [`postgres-compatibility-spike.html`](../../../server/public/reports/postgres-compatibility-spike.html).

The image is `postgres:16.15-bookworm` (pinned by digest) plus two extensions built from checksummed source:

- **Apache AGE 1.6.0** for graph queries, which replace SurrealDB's record links and graph edges.
- **pgvector 0.8.2** for embeddings.

Stock PostgreSQL supplies `citext`, `pg_trgm`, and full-text search. **DBOS** is the durable-execution target. It is an
application dependency, not part of the image.

## Rules

- **Pin by digest, in both places.** `k8s/postgres/` (the retained `navigator-postgres` StatefulSet) and
  `k8s/overlays/postgres-spike/` (the disposable `postgres-spike`) each carry the digest. Move them together. Never use
  a tag or `latest` in a shared environment.
- **The recipe mirrors upstream.** `Containerfile`, `LICENSE`, and `NOTICE` under `images/postgres/` must stay
  byte-identical to the pinned upstream commit. CI runs `cmp` on all three. To change the recipe, change upstream first,
  then move the commit pin in the workflow and the README together.
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
docker build -f images/postgres/Containerfile -t neon-law-postgres:local images/postgres
```

Run the published image in this worktree's KIND tier, beside the existing SurrealDB and Restate dependencies:

```bash
NAVIGATOR_KIND_DEPS_OVERLAY=k8s/overlays/kind-postgres-spike \
  cargo run -p cli -- dev worktree-env up --runtime kind --path "$PWD"
set -a; source .devx/env; set +a
kubectl --kubeconfig .devx/kubeconfig --context "kind-$NAVIGATOR_KIND_CLUSTER" \
  -n navigator rollout status statefulset/postgres-spike --timeout=180s
kubectl --kubeconfig .devx/kubeconfig --context "kind-$NAVIGATOR_KIND_CLUSTER" \
  -n navigator port-forward service/postgres-spike :5432
```

Read the loopback port from the port-forward output. The database is `smoke`, user `postgres`. Tear down only your own
tier with `cargo run -p cli -- dev worktree-env down --path "$PWD"`. [kind-local-dev](../kind-local-dev/SKILL.md) covers
the rest of the tier.

For the TLS and SCRAM shape of the retained StatefulSet, use `k8s/overlays/kind-postgres-persistence`.
`postgres-persistence.yml` shows the synthetic Secrets it needs.

## What is still open

Login roles and grants, the `web` opt-in, the migration Job, WAL archiving, base backups, and the restore drill are
defined in [`docs/postgres-kubernetes.md`](../../../docs/postgres-kubernetes.md). Failover, AGE and DBOS grants,
major-version upgrades, and the data migration off SurrealDB remain undecided. Do not pick one in passing. Raise it with
the user, or file it with [author-linear-issue](../author-linear-issue/SKILL.md).
