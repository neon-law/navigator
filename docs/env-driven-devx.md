# Env-driven orchestration — one config surface, three audiences

`NAVIGATOR_ENVIRONMENT` is the deployment-profile selector shared by the web application and shipping preflight. Exact
`dev` selects development; exact `production`, empty, or unset selects production. Any other value, case variant, or
surrounding whitespace is an error. Local KIND uses `dev`; every hosted deployment uses `production`, the three
`*-staging` rows included, because staging names a role in the release order, not an application value.

The profile controls fixture seeding. Every boot applies two seed layers that reach production — the canonical seed
every deployment shares, and the booting brand's own seed — and `dev` additionally and idempotently applies the
sample-matter fixture. Local boot also refreshes and stages each sample application before writing `.devx/env` for
`web`.

The brand layer is selected by the binary rather than by this variable: the brand layer seeds the Firm's own data, and a
white-label `tenant` boot seeds none; each brand declares its `BrandSeed` in the `Brand` value it hands to the shared
run loop. That split is why one deployment's postal identities never appear in another's database. All three layers are
idempotent, so a reset and recreate restores the same baseline.

A **deployment operator** owns Kubernetes, cloud accounts, secrets, domains, and these environment values. That person
is distinct from a Navigator application **admin**, whose database role grants application authorization but no
infrastructure access.

`hudson` owns orchestration in `deploy/src/devx/mod.rs`; one environment surface serves:

1. **Local dev:** `hudson dev up` against KIND; defaults need no `.env`.
2. **Every cloud deployment:** `hudson ops gcp setup` against that deployment's own project, then
   `hudson ops ship`. All six are provisioned and shipped by the same pipeline; there is no reduced staging substrate.
3. **OSS/multi-cloud:** supply cluster, namespace, overlays, and ports through `.env`, without Rust edits.

## The seam: one `KindConfig`, resolved once

`KindConfig` resolves all KIND/local values once, applies `DEFAULT_*` fallbacks, and is threaded through `up`, `deploy`,
`down`, `status`, environment rendering, and lifecycle helpers. Add each new knob only to this seam. `.env.example` is
the operator-facing list of variables and defaults.

## Naming: role, not provider

Variables are named `NAVIGATOR_<scope>_<thing>` so `.env.example` reads as one coherent table rather than two dialects:

- **Shared concepts get one var.** A Kubernetes namespace is the same idea in KIND and GKE, so it is
  `NAVIGATOR_K8S_NAMESPACE` (no `KIND`/`GKE` prefix).
- **Provider-specific concepts fork by scope.** The cluster name differs by provider — prod already has
  `NAVIGATOR_GKE_CLUSTER_NAME`, so the KIND cluster is `NAVIGATOR_KIND_CLUSTER`.
- **Overlay paths generalize.** `NAVIGATOR_KIND_OVERLAY` (full local stack) and `NAVIGATOR_GKE_OVERLAY` are the same
  idea at two scopes; a fork points either at its own kustomize overlay.

## Host ports

The host ports split into two categories with very different blast radius:

- **Port-forward ports:** PostgreSQL, SeaweedFS S3, and OpenObserve UI and OTLP ingress. The host application binds its
  HTTP and DBOS ingress listeners in the worktree’s allocated port slot.
- **Create-time NodePort mappings:** ingress HTTP/HTTPS and Rauthy, rendered into `k8s/kind-config.yaml`.

The CLI renders a temporary KIND config and changes only requested `hostPort` values. Port-forward changes, including
SeaweedFS, require no cluster recreation.

## Worktree isolation

`hudson dev worktree-env up` gives the checkout its own KIND cluster, port slot, and PostgreSQL database. `worktree-env
down` uses the persisted descriptor to release that checkout's environment. `worktree-env sweep` reports orphaned
clusters by default. With `--apply`, it removes only clusters whose worktrees are gone.

Tests cover resolved defaults, ports in `.devx/env`, KIND overrides, and the isolated database lifecycle.

## Related

- `AGENTS.md` — local development contract.
- [`cloud-operations.md`](cloud-operations.md) and `.env.example` — production environment surface.
- [`oss-install.md`](oss-install.md) — GCP provisioning conventions.
