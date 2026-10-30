# Forgejo on KIND

Forgejo runs in the local KIND tier beside PostgreSQL and Rauthy, with one Actions runner. It is opt-in: `dev up` and
`worktree-env up` do not start it. The same two commands run locally and in CI.

```bash
set -a; source .devx/env; set +a          # select this worktree's KIND tier
cargo run -p cli -- dev forgejo up         # converge Forgejo into the tier
cargo run -p cli -- dev forgejo smoke      # prove it end to end
```

`up` leaves Forgejo at `http://localhost:3300/` (`NAVIGATOR_KIND_FORGEJO_PORT`) and PostgreSQL at `127.0.0.1:15432`
(`NAVIGATOR_KIND_POSTGRES_PORT`, TLS only) through port-forwards. A worktree tier's `.devx/env` moves both onto its
slot. Their pids are in `.devx/<service>-port-forward.pid`; re-run `up` after a restart or sleep. Sign in through the
tier's Rauthy as `forge-allowed@example.test` once the allowlist names `example.test` (see [Admission](#admission)).

## What `up` deploys

`up` applies [`k8s/overlays/kind/forgejo`](../k8s/overlays/kind/forgejo/kustomization.yaml) to the selected context and
namespace:

| Workload | Source | KIND difference |
| --- | --- | --- |
| `navigator-postgres` StatefulSet | `k8s/overlays/kind-postgres-persistence` | 1 GiB retained claim |
| `navigator-postgres-roles` Job | `k8s/postgres/access` | none |
| `navigator-forgejo-database` Job | `k8s/forgejo/database` | none |
| `navigator-forgejo` StatefulSet | `k8s/forgejo` | 1 GiB claim; Rauthy loopback sidecar |
| `navigator-forgejo-runner` Deployment | `k8s/forgejo/runner` | KIND only; no deployment overlay has it |

The server and the runner are pinned by digest: Forgejo 15.0.9 (`codeberg.org/forgejo/forgejo@sha256:caf1bca3…`) and
Forgejo Runner 13.2.0 (`data.forgejo.org/forgejo/runner@sha256:ca3d5eea…`). The PostgreSQL Jobs use the server's own
image digest; `cli/tests/staging_manifests.rs` fails when they drift.

The `navigator-forgejo-database` Job is the only Forgejo workload that signs in as the bootstrap superuser. It converges
the `forgejo` login (no superuser, role, database, replication, or RLS-bypass rights) and a `forgejo` database it owns,
revokes `PUBLIC` there, and grants it nothing on `navigator`. Forgejo connects with `verify-full`.

The first `up` generates synthetic credentials once and keeps them in the `navigator-forgejo-kind` Secret: the
PostgreSQL passwords, Forgejo's keys, and the runner's registration secret. Every other Secret is derived from it, so
re-running `up` converges instead of rotating. The PostgreSQL server certificate is a self-signed synthetic one,
generated once with `openssl`. The base `app.ini` is the deployment template rendered by the same code as `ops secrets
apply --service forgejo`, with three KIND differences: a loopback `ROOT_URL`, Actions on, and an instance signing key.
`up` generates that synthetic SSH key once into the `navigator-forgejo-signing` Secret and mounts it read-only; Forgejo
signs authorized initial commits, API edits and merges with it. The instance signing policy is `always` for these
operations; signing requires no separate human key or local second-factor enrollment. Branch protection still governs
which changes can reach `main`.

`up` also applies the Rauthy fixture, aligned to the tier's Rauthy port, so the `navigator-forgejo` client and the
synthetic smoke users exist. A bare cluster gets Rauthy itself. A changed fixture restarts Rauthy.

## Actions

The runner registers with a shared secret: `up` runs `forgejo forgejo-cli actions register` in the Forgejo pod and gives
the runner the matching connection in its `config.yml`. It offers one label, `self-hosted`, on the `host` executor. Each
job runs inside the unprivileged runner container: no Docker socket, no privileged mode, no Kubernetes API token. It is
not an isolation boundary between jobs, so it is a KIND and CI proof only. Deployment overlays keep Actions off until
Project workflows have isolated, ephemeral execution
([`forgejo-deployment.md`](forgejo-deployment.md#repository-policy-and-actions-migration)).

## Admission

Forgejo signs in through the tier's Rauthy with its own `navigator-forgejo` client. Registration follows the
`email_domain_allowlist` key of the `navigator-forgejo-admission` ConfigMap, read at each pod start. The KIND overlay
commits it empty, so nobody can create an account until it names a domain:

```bash
kubectl -n navigator patch configmap navigator-forgejo-admission \
  -p '{"data":{"email_domain_allowlist":"example.test"}}'
kubectl -n navigator delete pod navigator-forgejo-0
```

Re-running `up` empties it again. People sign in through the identity provider without a separate Forgejo password or
second-factor prompt. KIND uses synthetic Rauthy identities for the same OIDC contract that deployed Google sign-in
uses. The rules are in [OIDC](oidc.md#forgejo).

## The smoke

`dev forgejo smoke` uses only synthetic identities, repositories, and credentials. In order, it proves:

1. **PostgreSQL.** Forgejo holds TLS connections only. The `forgejo` login is unprivileged, owns exactly one database,
   reaches it with `verify-full`, cannot connect to `navigator`, and cannot connect without TLS.
2. **Closed registration.** Local signup renders no form and creates no account. The anonymous admin API refuses. With
   the allowlist empty, a verified Rauthy identity reaches only the link-account page, and its signup there creates
   nothing.
3. **Admission.** With `example.test` allowed, `forge-denied@example.org`, the suffix lookalike
   `forge-lookalike@notexample.test`, and the unverified `forge-unverified@example.test` get no account.
   `forge-allowed@example.test` gets exactly one, not an administrator, bound to its Rauthy subject.
4. **Provider sign-in.** The OIDC session opens account settings directly, and the admitted account can use its API
   credential and git over HTTP without local second-factor enrollment.
5. **Git and Actions.** The account creates a private repository, clones it, and pushes a commit with a workflow over
   HTTP. The workflow runs to success on the KIND runner.
6. **Merge policy.** The smoke reports drift without writing, protects `main`, applies squash-only settings with zero
   required approvals, verifies read-back convergence, detects and repairs a removed required check, and rejects
   unsigned direct pushes. The same account is briefly a site administrator to prove direct push, force-push, and
   deletion of `main` still fail. A synthetic pending check supplies this phase's context.
7. **Squash merge.** The account registers a synthetic OpenPGP key, proving possession through Forgejo's token, and
   creates `merge-proof`, whose initial commit Forgejo signs. One signed commit adds three synthetic workflows from
   [`cli/src/devx/forgejo/fixtures/`](../cli/src/devx/forgejo/fixtures): `gate` on pull requests, `publish` on pushes to
   `main`, and `bot-push` on dispatch. Three signed pull requests open: `red-proof`, whose gate fails; a draft; and
   `green-proof`. The required check is the context the gate run posts, `gate / gate (pull_request)`. Scheduling refuses
   before `main` requires it; reconciliation then reports drift, applies, and reruns with none. `ops forgejo
   schedule-open` schedules the red and green pull requests while green's gate is pending, and skips the draft. Green
   then merges with no further action: `main` is one Forgejo-signed squash commit on the previous head, titled and
   bodied from the pull request; `green-proof` is deleted; and `publish` succeeds on the merge commit. Red's gate fails;
   an immediate merge and the repository owner's forced merge each answer 405, and it stays open. A site administrator's
   forced merge is not refused: Forgejo 15.0.9 lets an instance administrator skip branch protection whatever
   `apply_to_admins` says. `ops forgejo auto-merge` refuses the draft until its `WIP:` prefix is removed. Its gate has
   already passed by then, so the command merges it immediately as the same signed squash commit. Signed direct pushes
   by the account and by a site administrator are refused as pushes to a protected branch, and force-push and deletion
   fail. The `bot-push` job clones with its own Actions token and must fail to push to `main`. A final reconcile is a
   no-op.
8. **Repository path.** A synthetic Firm (`Example Law PC`) and its Project (`sample-litigation`) are seeded in an
   in-memory store. The Firm's organization and the Project's private repository are created through the API, and the
   API and the database name it exactly `example-law-pc/sample-litigation`, the `<firm-slug>/<project-code>` that
   `store::firms::forgejo::repository_path` returns. Organization creation is closed to regular accounts, so the
   admitted smoke account is a site administrator for this phase only. The phase deletes the repository and organization
   when it finishes.
9. **Retention.** The pod is deleted, then scaled to zero and back with the allowlist emptied in between. The pod UID
   changes and the claim UID does not. The commit, a fresh clone, and the repository row survive. The account still
   signs in directly through the provider with the same issuer subject.

It ends with the allowlist empty and purges its accounts at the start of the next run.

## CI

[`forgejo-postgres.yml`](../.github/workflows/forgejo-postgres.yml) renders the GKE overlays, creates a task-owned KIND
cluster from [`k8s/kind-forgejo-config.yaml`](../k8s/kind-forgejo-config.yaml), builds the CLI, and runs `dev forgejo
up` and `dev forgejo smoke`. The cluster maps Rauthy's NodePort to `localhost:30080`, the issuer the tier advertises. It
runs on changes to the Forgejo, PostgreSQL, and Rauthy KIND manifests, the GKE Forgejo overlays, or
`cli/src/devx/forgejo/`. It is not a required check.

To reproduce it without a tier, create the same cluster and point the CLI at it. If `localhost:30080` is taken, map
another host port and set `NAVIGATOR_KIND_RAUTHY_PORT` to it:

```bash
kind create cluster --name forgejo-smoke --config k8s/kind-forgejo-config.yaml --kubeconfig /tmp/forgejo-kind.config
export KUBECONFIG=/tmp/forgejo-kind.config NAVIGATOR_KIND_CLUSTER=forgejo-smoke
kubectl --context kind-forgejo-smoke create namespace navigator
cargo run -p cli -- dev forgejo up
cargo run -p cli -- dev forgejo smoke
kind delete cluster --name forgejo-smoke
```

GKE storage provisioning, a deployment's identity-provider registration, and runner isolation for Project workflows need
their own evidence; this job does not claim them.
