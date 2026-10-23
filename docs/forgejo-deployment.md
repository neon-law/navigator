# Forgejo beside Navigator

The opt-in `k8s/forgejo/` StatefulSet runs Forgejo 15.0.9 LTS, rootless, pinned to the published multi-platform image
digest. It connects to the existing `navigator-postgres` service: the PostgreSQL 16.15 / AGE 1.6.0 / pgvector 0.8.2
image documented in [PostgreSQL Kubernetes](postgres-kubernetes.md). Forgejo owns a separate `forgejo` database and
login. It never uses Navigator's database or bootstrap superuser. Limit its connection pool to ten. Forgejo manages its
own schema migrations; Navigator's future Rails-style migrations must not own those tables.

The local KIND tier runs the same manifests, plus an Actions runner, through `navigator dev forgejo up`; see [Forgejo on
KIND](forgejo-kind.md). GKE release workflows deploy PostgreSQL, converge its client roles and Navigator migrations,
prepare Forgejo's separate database, and wait for Forgejo readiness before applying Navigator workloads.

## Prerequisites and application

For each installation, the Deployment Operator must:

1. Provision PostgreSQL's encrypted service credentials and the deployment coordinates listed below. The release
   workflow checks these prerequisites and the required Kubernetes objects before applying any service manifests.
2. Provision the retained PostgreSQL StorageClass, backup target, and required service Secrets/ConfigMaps. The ordered
   release applies the PostgreSQL overlay and waits for its StatefulSet before continuing.
3. Provision `navigator-forgejo-postgres-ca` with `ca.crt`, the root CA that signs PostgreSQL's server certificate.
   Its certificate must cover the database host used by Forgejo. `verify-full` and `PGSSLROOTCERT` verify that identity.
4. Set `NAVIGATOR_FORGEJO_ROOT_URL` in the deployment's `config.toml`, then use the service-only
   `navigator ops secrets apply --deployment <name> --service forgejo --deployments-dir .` path documented in
   [`deployment-secrets.md`](deployment-secrets.md). It renders `app.ini` from `k8s/forgejo/app.ini.example` and applies
   only Forgejo's mounted-file Secrets; it does not expose these values through web/workflow env vars or deploy Forgejo.
   Do not commit the rendered configuration. The OAuth2 JWT signing secret is included because Forgejo cannot generate
   it into the read-only config mount.
5. Register Forgejo's own client at the installation's identity provider (see
   [Federated identity](#federated-identity)), then add its secret and client id to the same service path.
6. When a release is shipped, it applies PostgreSQL, runs the client-role and Navigator-migration jobs, then runs the
   `forgejo/database/<env>` overlay. That job converges Forgejo's dedicated login and database; it is the only Forgejo
   job that uses the bootstrap superuser. The full Forgejo overlay is applied only after the database job completes,
   followed by Navigator workloads. The Forgejo claim is separate from PostgreSQL's claim and retained on scale-down or
   deletion.
7. Point the host of `NAVIGATOR_FORGEJO_ROOT_URL` at the deployment's `NAVIGATOR_GATEWAY_IP` in DNS. The base exposes a
   cluster-local HTTP Service, with SSH disabled. On GKE, `ops ship` derives that host from the root URL, which must be
   a bare `https://<host>/`, and adds it to the shared `navigator-web-gke` Ingress. The host routes to
   `navigator-forgejo:3000` behind its own `navigator-forgejo` ManagedCertificate. The Forgejo overlay marks the Service
   as a NEG backend. The `navigator-forgejo` BackendConfig health-checks `/api/healthz` and allows ten-minute responses
   for large clones and pushes. The load balancer terminates TLS and forwards the external scheme, and Forgejo builds
   its links from `ROOT_URL`.

Check the `forgejo` login's grants whenever Navigator adds tables: sharing a server is not permission to read
application data.

The containers run as UID/GID 1000, drop capabilities, mount secrets read-only, and have no Kubernetes API token. Two
init containers run before Forgejo starts: `admission` renders the effective `app.ini` from the base config and the
allowlist, and `identity` migrates the schema, reserves the first administrator seat, and converges the OIDC source. Git
repositories, indexes, queues, and other local Forgejo state live on the retained `/var/lib/gitea` claim. Its PostgreSQL
metadata lives on PostgreSQL's separate retained claim. Backups must capture both consistently; keeping one PVC is not a
complete Forgejo recovery plan. Do not delete either claim or its namespace during a rollout.

## Federated identity

Forgejo signs in through the installation's existing OIDC issuer with its own client and callback,
`https://<forge-host>/user/oauth2/<provider>/callback`. Each deployment supplies its provider name and discovery URL
through the coordinates used to render the `navigator-forgejo-oidc` ConfigMap. The client id and secret are
environment-owned. Sharing the issuer does not permit reusing Navigator's client secret or its callback registration.

Every account must enroll a TOTP second factor, and password sign-in, local signup, and account linking stay off. The
email-domain allowlist is the `email_domain_allowlist` key of the `navigator-forgejo-admission` ConfigMap; the overlays
do not commit it, so registration stays closed until a sync from the database's Brand domains writes it. The full rules
are in [OIDC](oidc.md#forgejo). Forgejo accounts and repository rights do not grant Navigator Project participation, and
the reverse is also true.

### Navigator document-check tokens

Forgejo Actions can exchange a signed ID token for a short-lived, read-only Navigator session at `POST
/auth/ci/forgejo/document-token`. Enable this path only in the Navigator deployment with all three settings:

- `NAVIGATOR_FORGEJO_ACTIONS_OIDC_ISSUER`: the exact Forgejo installation
  issuer, including an installation path prefix and `/api/actions`.
- `NAVIGATOR_FORGEJO_ACTIONS_OIDC_AUDIENCE`: a deployment-owned audience that
  the workflow requests explicitly when asking Forgejo for an ID token. `navigator project gate --ci` requests the
  Navigator base URL of `navigator.yaml`'s `project.host` (for example `https://www.example.com`, no trailing slash), so
  set this to that URL.
- `NAVIGATOR_FORGEJO_ACTIONS_OIDC_WORKFLOW`: the one trusted workflow path. Set it to `.forgejo/workflows/gate.yml`, the
  gate `navigator project scaffold` writes for a Forgejo-backed Project.

### The Project gate on Forgejo

With `NAVIGATOR_FORGE_BACKEND=forgejo` in the operator's shell, `navigator project scaffold` writes
`.forgejo/workflows/gate.yml` beside the GitHub `ci.yml` and `cd.yml`, and `navigator project repository sync-skills`
restores it in any checkout that carries `.forgejo/`. Forgejo Actions reads `.forgejo/workflows/` and ignores
`.github/workflows/` once it exists. The workflow is named `gate` with one job `gate`, so it posts `gate / gate
(pull_request)`, the context `main`'s protection requires. It runs on every pull request with `enable-openid-connect:
true`. It checks out the pull request's head, installs the release `navigator.yaml`'s `version:` pins from the public
distribution host (sha256-verified), and runs `navigator project gate --ci`.

The CLI tells the runners apart by `FORGEJO_ACTIONS=true`; Forgejo's runner also sets the `GITHUB_*` compatibility
variables. On Forgejo it requests the ID token from `ACTIONS_ID_TOKEN_REQUEST_URL` and exchanges it only at `POST
/auth/ci/forgejo/document-token`. It never asks for the seed capability, which has no Forgejo door. Forgejo 15.0.9 runs
a pull request on `refs/pull/<n>/head`, not GitHub's `/merge` ref, and both the CLI and the verifier accept that ref. A
fork's pull request gets no ID token from Forgejo, so its live check is inconclusive.

Forgejo 15.0.9 signs Actions ID tokens with RS256 by default (`[actions] ID_TOKEN_SIGNING_ALGORITHM`). It uses its own
key at `<APP_DATA_PATH>/actions_id_token/private.pem`, which it creates on first use, separate from the `[oauth2]` JWT
secret. forgejo-runner 13.2.0 exports the request URL and token to every step, empty when the job did not opt in. The
KIND server and runner therefore need no extra setting. The runner must reach the Forgejo `ROOT_URL` and the Navigator
host, and it needs `git`, `curl` or `wget`, `sha256sum` or `shasum`, and `tar`.

The endpoint is disabled when these settings are absent or incomplete; invalid or partial opt-in fails closed and logs a
deployment configuration error. Production issuers use HTTPS. Navigator fetches discovery metadata from
`<issuer>/.well-known/openid-configuration` and accepts a same-origin JWKS endpoint only. Keep the Forgejo issuer,
audience, and workflow path in trusted deployment configuration; a Project manifest or caller-supplied repository cannot
select them.

The base/KIND and provider-neutral staging web manifests, plus the GKE web environment replacement patch, read these
optional keys from a deployment-created `navigator-forgejo-actions-oidc` ConfigMap in the `navigator` namespace. The
repository commits the references, not the ConfigMap or trust values. Its `issuer`, `audience`, and `workflow` keys map
to the corresponding `NAVIGATOR_FORGEJO_ACTIONS_OIDC_*` settings above. Create or update that ConfigMap through the
deployment's approved configuration process, then roll out `navigator-web`; the process reads trust once during router
construction. The GKE Forgejo staging/production overlays do not create or enable this Navigator trust ConfigMap. Verify
the rendered web Deployment before provisioning it. Removing the ConfigMap leaves the optional references unset and the
route disabled.

This verifier follows the pinned Forgejo 15.0.9 source contract: issuer `TrimSuffix(AppURL, "/") + "/api/actions"`;
discovery at the issuer's `/.well-known/openid-configuration`; keys at `<AppURL>/api/actions/.well-known/keys`; RS256;
and signed `repository`, `repository_owner`, `event_name`, `ref`, `base_ref`, `workflow`, `workflow_ref`, `sha`,
`run_id`, and `run_attempt` claims. The verifier allows `push` and `workflow_dispatch` on `main`, plus `pull_request`
head refs (`refs/pull/<n>/head`, the ref Forgejo 15.0.9 runs a pull request on) targeting `main`, for the
deployment-selected workflow. It rejects other events, refs, issuers, audiences, signatures, and workflow identities. A
minted session is limited to the live Project whose registered repository URL matches both the Forgejo host/installation
root and the signed `owner/name`; it expires no later than the verified JWT and has only the existing `DocumentScope`.
It cannot mint the seed capability or publish Project content.

Forgejo 15.0.9's `GenerateGiteaContext` sets signed `ref_protected` to false unconditionally, so Navigator does not
interpret it as branch-protection evidence. This route authorizes only metadata/document reads; deployments must enforce
protected `main` and workflow review through their Forgejo policy. The pinned claims contain no immutable repository
identifier, so deleting and recreating the same owner/name at the same installation path cannot be distinguished here.
Replay detection hashes the exact signed JWT in process memory until its expiration. It rejects repeated exchanges at
that process, but does not claim replay prevention across replicas or restarts.

The implementation contract is grounded in the pinned [Forgejo 15.0.9 OIDC claim
construction](https://codeberg.org/forgejo/forgejo/src/tag/v15.0.9/services/actions/context.go), [custom claims and
subject construction](https://codeberg.org/forgejo/forgejo/src/tag/v15.0.9/services/actions/auth.go), [issuer and
ID-token signing](https://codeberg.org/forgejo/forgejo/src/tag/v15.0.9/routers/api/actions/id_token.go), [discovery and
JWKS handlers](https://codeberg.org/forgejo/forgejo/src/tag/v15.0.9/routers/api/actions/oidc.go), and [ID-token
integration test](https://codeberg.org/forgejo/forgejo/src/tag/v15.0.9/tests/integration/api_actions_id_token_test.go).
These checks prove the verifier against signed local fixtures matching that source contract; they do not claim a live
Forgejo Actions run or fleet-wide replay test.

## Repository policy and Actions migration

The current exact reconciliation command is `navigator ops github setup [repository]`, implemented in
`cli/src/devx/github_setup.rs`. It distinguishes public source from private Project repositories and checks CODEOWNERS,
required `ci` checks, signed commits, deletion/force-push protection, pull requests, resolved review conversations, and
squash-only merging. Port and independently test those outcomes against Forgejo before deprecating the GitHub command.
Where Forgejo cannot express a rule directly, fail closed with an explicit tested equivalent rather than silently
weakening policy. Preserve private Project defaults separately from open-source Navigator governance.

`navigator ops forgejo setup <owner/repository> --url <origin> --check-context <context> --dry-run` reads the repository
and its `main` protection and lists drift without writing. Remove `--dry-run` to protect `main`, apply squash-only
settings, and verify the readable controls. A second run reports that those controls already match. Additional
branch-protection rules require review before reconciliation or scheduling, so a wildcard rule cannot silently override
the checked `main` rule. `FORGEJO_TOKEN` supplies the credential; it travels in an authorization header. Select the
exact required check context from a real gate run on that installation. The reconciler requires checks, signed commits
and pull requests, applies protection to repository administrators (not to an instance administrator's forced merge),
requires zero approving reviews, and blocks merges while a review requests changes. Forgejo enforces that review state
at merge time, including after auto-merge has been scheduled.

`navigator ops forgejo auto-merge <owner/repository> --url <origin> --check-context <context> --pull-request <number>`
re-reads the protection and refuses drafts, closed pull requests, a base other than `main`, or policy drift. It requests
a squash merge when checks succeed, supplies the pull request's title and body, and requests head-branch deletion. When
the required context has already succeeded on the pinned head, it requests that merge immediately instead: Forgejo
15.0.9 re-evaluates a scheduled merge only on a later status, review, or base push, so a schedule placed after the gate
passed would wait indefinitely. Forgejo still enforces protection on the immediate merge. A successful response confirms
request acceptance; the public API has no scheduled-merge read-back field. Forgejo must have its signing key provisioned
before it can merge into a branch requiring signed commits.

Before scheduling, the command pages through the pull request's reviews and checks every reported code-review comment.
It proceeds only when Forgejo returns each comment's resolution state and every comment is resolved; missing counts,
truncated responses, or unknown response shapes keep auto-merge off. This is a point-in-time check when the request is
scheduled. Forgejo 15 does not expose a native protected-branch rule that rechecks conversation resolution as new
comments arrive, so this guard alone does not provide race-free unresolved-thread enforcement. Keep that limitation in
the migration gate until the installation has a tested mechanism that enforces the condition through merge time. The
separate rejected-review rule does not resolve this gap: it blocks a review that requests changes, but does not require
inline conversations to be resolved.

`navigator ops forgejo schedule-open <owner/repository> --url <origin> --check-context <context>` is a run-once operator
command for one explicitly selected repository. It discovers every open pull request before writing, rejects incomplete
pagination, changed reported totals, duplicate numbers, and malformed entries, then re-reads each eligible pull request.
It schedules only when the head is unchanged, the PR is open and targets `main`, the review conversations pass, and the
required-check and repository-policy guards pass. Forgejo receives the same merge request as `auto-merge`: scheduled
while checks are pending, immediate once the required check has passed. Drafts and non-`main` bases are reported as
skips; conflicts and per-pull-request failures are reported as structured JSON outcomes and return a nonzero exit
status. `FORGEJO_TOKEN` must come from the trusted operator host environment. Never pass it into a
pull-request-controlled workflow.

These commands prepare source support for the merge-policy portion of the migration. Installing a trusted PR-open event
trigger, Actions check identity and publication triggers, CODEOWNERS, merge-time resolved-thread enforcement, automatic
branch cleanup on closure, and fleet creation/cutover remain separate gates. Forgejo 15.0.9 accepts `allow_manual_merge`
and `autodetect_manual_merge` writes but omits them from repository reads; every explicit non-dry apply writes both off,
including when readable settings already match, and read-back convergence covers only the fields the API exposes. These
unverified controls require behavioral proof before cutover.

The KIND smoke proves the merge path against synthetic workflows, not the Project gate. A synthetic gate's own `gate /
gate (pull_request)` context becomes the required check. Then a ready, signed, green pull request scheduled by
`schedule-open` squash-merges itself. The squash commit is Forgejo-signed and carries the PR's title and body, and the
head branch is deleted. A `push` workflow then runs on the merge commit. A red pull request stays open through an
immediate merge and the owner's forced merge; a draft is refused until it is marked ready. Signed direct pushes by the
owner and an administrator, and an Actions job-token push, are rejected. See [Forgejo on
KIND](forgejo-kind.md#the-smoke). The run depends on an instance signing key, a GPG key and second factor for the
scheduling account, and signed history. No deployment overlay provisions an instance signing key yet, so a deployment
cannot merge into signed-commit `main` until one is provisioned. Forgejo 15.0.9 lets an instance administrator's forced
merge skip branch protection regardless of `apply_to_admins`
([`services/pull/check.go`](https://codeberg.org/forgejo/forgejo/src/tag/v15.0.9/services/pull/check.go)), and the KIND
run observed a site administrator force-merging a red pull request. Keep site administration out of operator and bot
identities until a tested control closes that path. The smoke does not run the Project gate itself: the KIND runner
cannot reach the port-forwarded `ROOT_URL` or a seeded Navigator, and the release binaries are built for x86-64 Linux. A
unit test checks that the generated gate posts the required context. The smoke also does not prove a trusted PR-open
trigger, a dedicated scheduling bot account, or branch deletion on closure without merge.

Each Firm is one Forgejo organization, named by its immutable `firm.slug`, and each Project it owns is one repository
beneath it, named by its immutable `code`: `<firm-slug>/<project-code>` (`store::firms::forgejo::repository_path`). See
[Firm](glossary/firm.md) for how the slug is derived, reserved, and kept unique. Brands are storefronts, not namespaces.
`navigator dev forgejo smoke` creates a seeded synthetic Firm's organization and Project repository and checks the API
returns exactly that path. Concurrent creation, moves, and tenant isolation still need tests. Forgejo is one parallel
service per installation, not a single central organization shared between installations.

Actions stays disabled in the deployment overlays. The KIND tier runs one runner (Forgejo Runner 13.2.0, pinned by
digest in `k8s/forgejo/runner/`) on the `host` executor to prove registration and a workflow run; it is not an isolation
boundary. Before enabling Actions in a deployment, establish isolated, ephemeral execution for Project workflows,
restrict repository write credentials to the owning Project, deny untrusted pull-request secret access, and test that
another Brand/Project cannot read its source or publish its artifacts. Do not mount the host Docker socket or use
privileged shared runners in the Navigator/PostgreSQL trust boundary. Preserve artifact provenance, workflow references,
retries, and existing Project publisher behavior during migration.

## Git bundle archive and restore

`navigator ops forgejo archive <local-mirror> <object-key> <receipt-path>` creates a bundle from a stable bare mirror,
uploads it with GCS's create-only generation precondition, downloads that exact generation, checks its byte count and
SHA-256, and restores every ref and the default branch into a fresh mirror before writing the receipt. Set
`NAVIGATOR_FORGEJO_ARCHIVES_BUCKET` to the private bucket and provide the operator's GCS credentials through the
standard GCS credential chain. The object key is an opaque caller-selected name. The receipt contains the key,
generation, digest, size, default branch, and ref hashes; keep it with the archive under the same access controls.

`navigator ops forgejo restore <receipt-path> <new-mirror-path>` downloads the receipt's exact generation, verifies the
digest and bundle integrity, and publishes a fresh bare mirror only after all ref hashes and the default branch match.
The object key and receipt data may disclose repository information; command status prints only whether verification
passed. Existing objects, receipts, and restore destinations are never replaced. The CLI operator command uses its
configured credential for both upload and read-back; the storage API accepts separate writer and reader capabilities for
a future split-identity caller. This path proves one invoked Git-bundle round trip and does not configure a schedule,
object retention, bucket lock, encryption key, fleet coverage, or Forgejo metadata/LFS recovery. Restore currently uses
atomic no-replace directory rename on Linux and macOS; other targets fail explicitly rather than risk replacing an
existing path.

## Evidence

[`forgejo-postgres.yml`](../.github/workflows/forgejo-postgres.yml) renders both GKE overlays and runs `navigator dev
forgejo up` and `smoke` on a task-owned KIND cluster: TLS and database isolation, closed registration, email-domain
admission, mandatory TOTP, git clone and push, an Actions run, and retention across pod deletion and scale-to-zero.
[Forgejo on KIND](forgejo-kind.md) lists every check. GKE storage provisioning, a deployment's identity-provider
registration, and Actions isolation require separate evidence.

Sources:

- [Article](https://nickcunningh.am/blog/how-to-setup-and-configure-forgejo-with-support-for-forgejo-actions-and-more)
- [Forgejo configuration](https://forgejo.org/docs/latest/admin/config-cheat-sheet/)
- [Database preparation](https://forgejo.org/docs/latest/admin/installation/database-preparation/)
- [Actions deployment security](https://forgejo.org/docs/latest/admin/actions/security/)
