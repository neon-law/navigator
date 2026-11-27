---
name: kind-local-dev
description: >-
  Manage task-owned KIND environments, ports, and services. Use for local orchestration, app previews, and browser
  checks.
---

# KIND-based local development

The `hudson` CLI (`deploy::devx`) drives the host-`web` loop and KIND dependencies. Fresh KIND tiers use PostgreSQL and
the web-hosted DBOS executor. Install Docker, KIND, `kubectl`, and `openssl`. Focused PostgreSQL test fixtures are
separate from that tier.

Store fixtures are described in [`docs/test-database.md`](../../../docs/test-database.md); KIND port mappings are in
[`k8s/kind-config.yaml`](../../../k8s/kind-config.yaml).

## Worktree-first

Every code change starts in a **New Worktree** in Codex or Claude. The worktree is the isolated checkout; a topic branch
is the PR's Git reference. Codex starts worktrees at a detached `HEAD`; Claude may create a branch. Both are normal.

Before the first edit, inspect `git worktree list --porcelain`: the current `pwd -P` must be a non-primary `worktree`
entry. If it is not, do not create a branch or worktree or edit files; stop and say so. Never hand-create another
checkout to repair it.

Fetch and rebase onto `origin/main` before the first edit and again before every push. Rebase, not merge (PRs squash,
merge commits are disabled), and sign it: an unsigned commit cannot enter the merge queue.

```bash
rtk git fetch origin
git rebase -S origin/main
```

Then run the CLI once with the PR topic. It attaches or creates that branch **in this worktree**, including a detached
Codex one. It creates a sibling `.worktrees/<topic>` checkout only when started from the primary checkout outside Codex
or Claude.

```bash
cargo run -p hudson -- dev worktree-env up --branch <topic>
```

## The default worktree loop

Once the task branch exists, use its isolated dependency tier when editing `web`:

```bash
cargo run -p hudson -- dev worktree-env up --path "$PWD"
set -a; source .devx/env; set +a
cargo run -p neon
```

The worktree reserves one private creation operation and creates its own PostgreSQL database. Repeated starts verify the
same namespace and database. Clean up a legacy worktree with the Hudson version that created it.

Parallel worktrees keep separate application and durable histories. Source `.devx/env` before every command targeting
this checkout; it contains the complete private application environment. A gitignored `.env` holds only optional live
sandbox configuration, which must not be silently substituted into a capture-only native harness.

```bash
cargo run -p hudson -- dev worktree-env status --path "$PWD"
cargo run -p hudson -- dev worktree-env down --path "$PWD"
```

`worktree-env down` removes this checkout's port-forwards, cluster or native tenants, and `.devx` state, and never
touches another worktree's. Run it at handoff: a leftover cluster keeps binding its slot's ports, and a leftover native
claim keeps its tenant alive until `sweep` finds it orphaned.

## Fresh KIND and compatibility tiers

`dev up` uses the same worktree-owned KIND selection:

```bash
cargo run -p hudson -- dev up --path "$PWD"
set -a; source .devx/env; set +a
cargo run -p neon
```

Fresh tiers provision PostgreSQL with TLS and application roles, Rauthy, SeaweedFS, ClamAV, and telemetry. `web` hosts
DBOS on its own allocated listener. Re-run startup to verify and resume the same resource and restore owned transports.
Unknown or partial authority refuses startup; do not delete an intent or replace retained history to force a fresh
start. Stop the owned application before an explicit schema upgrade; `--no-deps` does not migrate or rerun role grants.

| Shared surface | Port |
| --- | --- |
| Rauthy | `30080` |
| SeaweedFS | `30900` |
| PostgreSQL TLS | `15432` |
| Web, default brand | `3001` |
| Web, Delete Your Data | `3011` |

No local DNS serves the brand hostnames, so `web` answers each brand on its own port, not by `Host:` header. A worktree
selects a free slot in the `20000`–`21599` ranges (skipping slots held by stopped or orphaned clusters), and its three
brand ports move with that slot. Always read the values from that worktree's `.devx/env`.

PostgreSQL is the storage authority. KIND provisions the pinned retained PostgreSQL image, TLS certificate, and
application roles through the same fixture seam as Forgejo. `.devx/env` contains the application role and a loopback
`verify-full` connection; `.devx/postgres/ca.crt` is the trusted certificate. KIND maps its PostgreSQL NodePort to
loopback directly so closing one database session cannot interrupt another pooled connection. Keep these files private.

Fresh initialization records the creation-time Database Origin, then seeds through admitted application accessors. It
does not fabricate a Source receipt or import an empty legacy engine. The complete native development harness uses the
existing capturing/stub providers under its explicit profile guards; captures prove no external delivery or production
cutover.

### Captured compatibility and the deploy browser gate

Captured legacy bootstrap holds web and worker writers, verifies an actual strict Source export, and enrolls the
required PostgreSQL scopes before seeding. Existing valid enrollment is reused; nonempty or partial history requires
explicit recovery. It never resets retained history or replaces receipts.

The `.github/workflows/deploy.yml` browser gate provisions its native tier through the ordinary quickstart CLI, then
requires exact-image nonroot, native admission, persisted DBOS workflow replay across host/image restart, and all three
hydrated browser suites. Independent practice previews use owned PostgreSQL fixtures. Explicit compatibility suites
retain their own Source/Restate fixtures; a focused local pass does not certify the full gate.

Forgejo is opt-in: `cargo run -p hudson -- dev forgejo up` reuses the retained PostgreSQL server and adds Forgejo and an
Actions runner to the sourced tier, signing in through Rauthy; `dev forgejo smoke` proves it. See
[`docs/forgejo-kind.md`](../../../docs/forgejo-kind.md).

## Verification

Start `web` with the correct `.devx/env`, then run the browser and accessibility gate. It downloads and caches the
pinned Chrome for Testing build, starts ChromeDriver on a free port, grants Lawyer, and runs all three suites with
`NAV_REQUIRE_HARNESS=1`. It reads the base URL from the sourced environment; override it only for a topology it did not
generate.

```bash
cargo run -p hudson -- dev browser-e2e
cargo run -p hudson -- dev browser-e2e --base-url http://localhost:3001
```

For the explicit in-cluster compatibility demo from published images:

```bash
cargo run -p hudson -- dev worktree-env up --demo
```

Optionally add `--tag YY.M.D`; the ingress is at <http://localhost:8080>.

## Troubleshooting and cleanup

```bash
kubectl --namespace navigator get pods
kubectl --namespace navigator describe pod <name>
kubectl logs --namespace navigator <name> --all-containers --tail=100
```

Keep task-owned resources while the task is active. At handoff, use `hudson dev worktree-env down --path "$PWD"` to
remove only that worktree's cluster, transports, and state. Never prune Docker volumes without explicit approval. `cargo
run -p hudson -- dev down --path "$PWD"` is a deliberate teardown, not a way to repair unknown retained authority. See
[`docs/agent-workflows.md`](../../../docs/agent-workflows.md#resource-cleanup).

On Codex-provisioned macOS worktrees, Gatekeeper may open repeated `Verifying "<binary>"…` windows the first time each
freshly built executable runs (the sandboxed host stamps `com.apple.provenance` on every executable Cargo writes).
Builds are unaffected. Take no action: do not strip the attribute, disable Gatekeeper or SIP, notarize test binaries, or
wrap Cargo in a shell script. Tracked in [navigator#570](https://github.com/neon-law-source-code/navigator/issues/570).

## The load-bearing rules

- **Use the CLI's watching server for interactive previews.** Reuse the configured worktree environment, source
  `.devx/env`, then run `cargo run -p hudson -- dev serve`. Rust and catalog saves rebuild and restart the server;
  static assets refresh the browser without compilation. Keep it running through the edit-and-check loop on the brand
  port from `.devx/env`. Follow [web-preview](../web-preview/SKILL.md) to verify automatic refresh and the page.
- **We standardize on KIND.** Do not install another local-Kubernetes flavor (minikube, k3d, Docker Desktop k8s); the
  manifests, port mappings, and `cli::devx` orchestration all assume KIND.
- **Fresh KIND tiers retain their PostgreSQL origin.** PostgreSQL storage survives pod restarts. Repeated startup
  verifies the same database and creation evidence; it does not replace an unknown or partial database. Memory-backed
  SeaweedFS is a disposable single-node store.
- **`web` uses the task-owned PostgreSQL TLS mapping.** Source the generated environment with
  `set -a; source .devx/env; set +a`; it carries the owned port, database, application login and CA. Compatibility
  Source forwarding belongs to its explicit topology, not fresh native KIND.
- **Screenshots go to `/tmp/navigator-screenshots/`, never the repo** (`mkdir -p` first).

## Anti-patterns

- Editing `k8s/` manifests for a one-off debug flag. Use `kubectl patch` or a kustomize overlay for a transient change.
- Expecting the host cluster and CI's KIND cluster to behave identically. The host has whatever you `kind load`ed; CI
  starts clean.
- Using `:latest` image tags. Tag with a content hash or `YY.MM.DD` so `kind load` and rollout are deterministic.
- `kubectl delete pod` to "fix" a `CrashLoopBackOff`. That hides the failure; read `kubectl logs --previous`, then
  `kubectl describe pod`.

## Boundaries

- The browser half of the local loop (drive Chrome, screenshot, verify a UI change): [[web-preview]].
- Use the authorization, Rauthy/OIDC, and durable-execution skills for their service boundaries.
