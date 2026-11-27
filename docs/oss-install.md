# Installing Neon Law Navigator on your own cloud

Neon Law Navigator's canonical build is just **`cargo build` + `docker build`**. Nothing in the workspace's default
surface assumes a particular cloud account, project ID, OAuth client, or domain. To run it against production traffic
you assemble a runtime (Kubernetes, ECS, or plain Compose), PostgreSQL, and the configured provider integrations.
Existing deployments retain compatibility services until their verified handover and retained-work disposition are
complete. Environment variables are documented in [`../.env.example`](../.env.example).

This page walks the end-to-end setup against **GCP**, because the workspace ships a working example overlay for that
path. The same shape works against EKS, AKS, or self-hosted Kubernetes; see [`multi-cloud.md`](multi-cloud.md) for those
routes.

## Who this page is for

This walkthrough provisions a Navigator deployment on Google Cloud. It is written for the Firm's own operators and for
anyone else standing Navigator up under the licence.

Navigator's first-party software is licensed under `MIT OR Apache-2.0` ([LICENSE](../LICENSE)); commercial deployment is
permitted under either license's terms. Third-party software and assets retain their own terms. The licenses disclaim
warranties and limit liability subject to applicable law and written agreements; deployment still requires operational
and security review.

Rename your deployment through the brand manifest rather than by editing sources: the NEON LAW marks are not licensed
here, and that seam exists so a fork does not have to patch views to drop them.

## 0. Prerequisites

Grouped by what you are actually doing. Only the first two groups are needed to build and test; nothing here requires a
cloud account. Every entry below is here because a command on this page, or the CLI those commands reach, actually
invokes it — the list is derived from call sites rather than from the CI runner's image inventory.

**To build and test:**

- **Rust** — pinned to 1.98.0 by the workspace's `rust-toolchain.toml`, so installing [`rustup`](https://rustup.rs) is
  enough: it reads that pin and fetches the right toolchain on your first `cargo` invocation.
- **A C/C++ toolchain and linker for your host** — the Xcode command line tools on macOS, `build-essential` or
  equivalent on Linux. Several dependencies compile native code. Navigator is built for Macs and containers in the
  cloud; Windows is not supported.
- **`cmake`** — `libz-ng-sys` shells out to it from its build script, and it sits on the default workspace surface via
  `archives` → `iceberg` → `parquet` → `flate2`. It is not optional or feature-gated: without it, step 1 below fails on
  a clean machine.
- **`git`**.

**Test tooling, at the pinned versions:**

```bash
cargo install cargo-nextest --locked --version 0.9.140   # pinned by .github/workflows/deploy.yml
cargo install cargo-llvm-cov --locked --version 0.8.7    # pinned by .github/workflows/ci.yml
```

The versions matter rather than being tidiness: the 92.0% workspace line floor is a merge gate, so a drifting version
measures against a different denominator than CI does. `--locked` is required, not advisable — `cargo-nextest` fails to
compile without it.

**To build images and deploy:**

- **Docker**, for image builds. The test suite needs none.
- **`kubectl`** and **`gcloud`** — only if you are following the GCP example below. Kustomize needs no separate install:
  every overlay in this repository is rendered through `kubectl apply -k` or `kubectl kustomize`, which is the copy of
  Kustomize built into `kubectl`. Nothing here shells out to a standalone `kustomize` binary.
- **A domain you control**, with the ability to set A records.

**To run the local KIND cluster** (`hudson dev up`, and the per-worktree `dev worktree-env up`):

A fresh tier uses PostgreSQL and DBOS without a SurrealDB server, Restate broker, or separate workflow worker. It
requires Docker, `kubectl`, `kind`, and `openssl` (`deploy/src/devx/orchestrate.rs:133`); quickstart also uses `git`,
`ssh-keygen`, Rust, and `cmake`. Its Ubuntu job installs KIND v0.33.0 (`.github/workflows/quickstart.yml:25`).

Existing legacy tier descriptors retain their compatibility topology. Their tool gate also requires Helm
(`deploy/src/devx/orchestrate.rs:43`) for the Restate Operator install (`deploy/src/devx/orchestrate.rs:1102`). The
native deploy browser gate and standalone quickstart job both pin KIND v0.33.0. The deploy gate provisions through the
ordinary quickstart CLI and then tests the exact built application image without Source, Restate, or a separate workflow
worker. The compatibility topology remains explicit in the CLI.

Quickstart builds and stages the current checkout's Dioxus client before creating the KIND tier or starting the server
watcher. It uses the debug profile locally and rebuilds incrementally on each launch; an existing bundle is not treated
as current. Install the pinned client tool and WebAssembly target first on macOS or Linux:

```bash
cargo install dioxus-cli --version 0.7.10 --locked
rustup target add wasm32-unknown-unknown
```

Client and server compilation run sequentially to bound peak memory. A missing or incompatible `dx` fails before
quickstart creates dependency resources.

Fresh provisioning reserves one private creation operation in `.devx/postgres/native-origin.json`, creates its unique
PostgreSQL database, and verifies its Database Origin before seeding through the application role. Repeated starts
verify the same namespace and database. An unknown or partially initialized database requires explicit owned-resource
recovery; startup never replaces it with a new empty database. Stop the owned app before a schema upgrade. The
provisioner records the upgrade operation before migration so an interrupted upgrade resumes that same operation.

The local capture providers require the Dev CI harness, simulated fixtures, embedded hosting, and the complete native
caller matrix. They retain the actual handler registrations while capturing external effects locally. Explicit live
provider credentials or destinations conflict with this mode. Billing Canary and Reconcile Invoices timer loops are off
by default; their manual handlers remain registered. This local setup does not transfer retained production work or
prove recovery of an existing deployment.

[`kind-local-dev`](../.agents/skills/kind-local-dev/SKILL.md) documents what that lane brings up, the ports it claims,
and how to tear it down again.

**Optional, for the browser suites:** a matching Chrome/ChromeDriver pair. The browser and e2e tests self-skip without
one, so this can wait until you need it.

## 1. Clone and build

```bash
git clone https://github.com/neon-law/navigator.git   # or your fork
cd navigator
cargo build --workspace          # pulls dependencies + compiles every crate
```

Building needs no cloud account. Follow [test database](test-database.md#run-tests) for focused PostgreSQL fixtures and
the explicit compatibility-inclusive test commands. Database-backed tests require their respective fixtures; an
unfiltered workspace run includes compatibility tests.

## 2. Configure your `.env`

Copy the template and start filling values:

```bash
cp .env.example .env
```

### Brand bundle

The deployment operator, not a Navigator application admin, owns a brand bundle. Build the deployer's private manifest
and logos into one directory:

```bash
cp deploy/tests/fixtures/navigator.example.yaml navigator.yaml   # then edit: names, emails, domain, logos
cargo run -p hudson -- ops rebrand build --file navigator.yaml --out /tmp/brand-bundle
cargo run -p hudson -- ops rebrand verify --dir /tmp/brand-bundle
```

`deploy/tests/fixtures/navigator.example.yaml` is the worked manifest, and it lives under `tests/` because that is where
it is proven: `cli/tests/brand_example_manifest.rs` builds and verifies a bundle from it on every run, so the example
you copy is one the CLI accepts.

Branding is Neon Law by default. To ship under your own identity, set `NAVIGATOR_CUSTOM_BRANDING` to the bundle
directory — `/etc/navigator/brand` for the Kubernetes mount, or any path for a non-Kubernetes install or test. When it
is set the bundle must load and validate or the process fails closed; unset means the built-in Neon Law identity. The
bundle contains only identity metadata and deployment static files. It never contains client documents, generated PDFs,
public form blanks, archives/exports, Git LFS data, or `NAVIGATOR_ASSET_BASE_URL` content. For a private Kubernetes
overlay, add [`web-brand-bundle.yaml`](../examples/deploy/k8s/gke/patches/web-brand-bundle.yaml) as the patch (it sets
`NAVIGATOR_CUSTOM_BRANDING` and mounts the bundle); replace its ConfigMap volume source with a Secret or PVC when
appropriate; both web and workflow-worker mounts remain read-only.

The `portal` workspace crate exposes the shared application core: the authenticated portal and Owner/Admin/Lawyer/Clerk
surfaces, JSON API and documentation, MCP, A2A, and OIDC. Its `portal::router` entry point is the seam brand hosts use
instead of copying these routes, keeping the firm's deployment and any white-label deployment on the same application
and documentation contract.

The minimum to boot `web` against a real store:

```dotenv
NAVIGATOR_SURREAL_ENDPOINT=wss://surreal.internal.example
NAVIGATOR_SURREAL_NAMESPACE=navigator
NAVIGATOR_SURREAL_DATABASE=navigator
NAVIGATOR_SURREAL_USER=<secret-backed username>
NAVIGATOR_SURREAL_PASSWORD=<secret-backed password>
NAVIGATOR_ENVIRONMENT=dev                       # exact; production/empty/unset means production
NAVIGATOR_STORAGE_BACKEND=s3                   # SeaweedFS or a conforming S3 endpoint
NAVIGATOR_STORAGE_ENDPOINT=https://s3.internal.example
NAVIGATOR_STORAGE_REGION=us-east-1
NAVIGATOR_STORAGE_BUCKET=navigator-documents
NAVIGATOR_STORAGE_ACCESS_KEY=<secret-backed access key>
NAVIGATOR_STORAGE_SECRET_KEY=<secret-backed secret key>
SESSION_SECRET=<32 bytes from `openssl rand -hex 32`>
OAUTH_ISSUER_URL=https://accounts.google.com
OAUTH_CLIENT_ID=<your client id>
OAUTH_CLIENT_SECRET=<your client secret>
OAUTH_REDIRECT_URI=https://www.your-domain.example/auth/callback
RESTATE_BROKER_URL=<your Restate Cloud or in-cluster operator URL>
SENDGRID_API_KEY=<key>
SENDGRID_FROM_EMAIL=<verified non-production sender>
SENDGRID_INBOUND_SECRET=<random secret>
SENDGRID_EVENTS_SECRET=<random secret>
SENDGRID_EVENTS_PUBLIC_KEY=<SendGrid signing key>
NAVIGATOR_CREDENTIAL_ENVIRONMENT=dev
DOCUSIGN_BASE_URL=https://demo.docusign.net/restapi
DOCUSIGN_ACCOUNT_ID=<demo account id>
DOCUSIGN_ACCESS_TOKEN=<demo token or configure the JWT variables>
DOCUSIGN_HMAC_KEY=<demo Connect HMAC key>
DOCUSIGN_WEBHOOK_SECRET=<random secret>
```

Every variable is documented and inventoried inline in `.env.example`. Exact `NAVIGATOR_ENVIRONMENT=dev` selects the
development profile, which local KIND uses; exact `production`, empty, or unset selects production, which every hosted
deployment uses, and every other value — including `staging` and `test` — is rejected without trimming or case folding.
Production requires hosted GCS with no emulator endpoint. A normal dev deployment requires real non-production SendGrid
and DocuSign demo credentials. Only `NAVIGATOR_CI_HARNESS=1` may use fakes.

### Third-party integrations: a separate vendor account per environment

Some integrations talk to an external SaaS that issues real, billable, or legally binding actions — DocuSign
(e-signature) today, Xero (accounting/billing) next. For these, create **two accounts with the vendor**: a
development/sandbox account you use locally and in CI, and a production account you use only in prod. The sandbox
account keeps test data — unsigned envelopes, draft invoices — out of your real books and off real signers.

`NAVIGATOR_ENVIRONMENT` selects only deployment wiring; it is not a general runtime mode. Credential sources remain
separate:

- `.env` holds your **sandbox** credentials and is auto-loaded on startup — local dev and tests run against the vendor's
  sandbox by default.
- `.env.production` holds your **production** credentials. It is gitignored by the `.env.*` rule; never commit it. To
  run against production locally, source it over the defaults before launching the binary:

  ```bash
  set -a; source .env.production; set +a
  ```

Both sources use the same application variable names. `NAVIGATOR_CREDENTIAL_ENVIRONMENT` stamps the credential set and
must match the deployment selector, preventing staging and production credentials from crossing. A fake provider is
available only to the explicit CI harness. See [`third-party-integrations.md`](third-party-integrations.md).

## 3. (GCP path) Provision the cloud resources

The `navigator` CLI ships a one-shot, idempotent provisioner for the GCP-side infrastructure — VPC, five GCS buckets
(`assets`, `documents`, `exports`, `logs`, `applications`), runtime identity, GKE Autopilot cluster, Fleet membership,
Gateway static IP, and the deployment's KMS key. It provisions no database: the store is SurrealDB, and you bring your
own instance.

```bash
gcloud auth application-default login
cargo run -p hudson -- ops gcp setup \
  --project-id YOUR_PROJECT_ID \
  --public-base-url https://www.your-domain.example \
  --region us-west2 \
  --cluster-name navigator-prod \
  --vpc-name navigator-vpc \
  --gateway-ip-name navigator-gateway-ip
```

Each flag has a sensible default (see `cargo run -p hudson -- ops gcp setup --help`) and falls back to a `NAVIGATOR_*`
env var if unset. Pass `--dry-run` first to print the exact REST calls / `gcloud` invocations the run will emit.

The subcommand generates no credential and prints no secret. Your store credentials come from your SurrealDB provider
instead.

## 4. Adapt the example overlay

Copy [`examples/deploy/k8s/gke/`](../examples/deploy/k8s/gke/) to a private location (or to your own kustomize overlay
branch) and substitute the placeholders documented in [`examples/deploy/README.md`](../examples/deploy/README.md):

```bash
cp -r examples/deploy/k8s/gke /tmp/my-overlay
find /tmp/my-overlay -type f \( -name '*.yaml' -o -name '*.yml' \) -print0 \
  | xargs -0 sed -i \
      -e 's|YOUR_PROJECT_ID|acme-prod-1234|g' \
      -e 's|YOUR_PROJECT_NUMBER|987654321098|g' \
      -e 's|YOUR_OAUTH_CLIENT_ID_BROWSER|...|g' \
      -e 's|YOUR_OAUTH_CLIENT_ID_GEMINI|...|g' \
      -e 's|your-domain.example|acme.com|g'
```

Create the runtime Kubernetes Secret (out-of-band — `kubectl create secret` keeps the values out of the manifest tree):

```bash
kubectl -n navigator create secret generic navigator-web-secrets \
  --from-literal=NAVIGATOR_SURREAL_PASSWORD='...' \
  --from-literal=OAUTH_CLIENT_SECRET='...' \
  --from-literal=SESSION_SECRET="$(openssl rand -hex 32)" \
  --from-literal=RESTATE_BROKER_URL='...' \
  --from-literal=RESTATE_AUTH_TOKEN='...' \
  --from-literal=SENDGRID_API_KEY='...' \
  --from-literal=SENDGRID_FROM_EMAIL='noreply@your-domain.example' \
  --from-literal=SENDGRID_INBOUND_SECRET="$(openssl rand -hex 32)" \
  --from-literal=SENDGRID_EVENTS_SECRET="$(openssl rand -hex 32)" \
  --from-literal=SENDGRID_EVENTS_PUBLIC_KEY='...' \
  --from-literal=NAVIGATOR_CREDENTIAL_ENVIRONMENT=production \
  --from-literal=DOCUSIGN_BASE_URL='https://your-account.docusign.net/restapi' \
  --from-literal=DOCUSIGN_ACCOUNT_ID='...' \
  --from-literal=DOCUSIGN_ACCESS_TOKEN='...' \
  --from-literal=DOCUSIGN_HMAC_KEY='...' \
  --from-literal=DOCUSIGN_WEBHOOK_SECRET="$(openssl rand -hex 32)"
```

Label the Secret so infrastructure inspection and runtime credential identity agree:

```bash
kubectl -n navigator label secret navigator-web-secrets navigator.neonlaw.org/environment=production
```

Apply:

```bash
kubectl apply -k /tmp/my-overlay
```

## 5. Build and push the image

The `images/Containerfile.neon` recipe produces the multi-stage `neon-server` image (build context is the repo root).
Tag it for your registry and push:

```bash
TAG=$(git rev-parse --short HEAD)
docker build -f images/Containerfile.neon -t my-registry/neon-server:$TAG .
docker push my-registry/neon-server:$TAG
```

Then roll your cluster onto the new tag — `hudson ops ship --tag <TAG>` renders the embedded manifest tree from your
`NAVIGATOR_*` env and applies it, or set the tag via kustomize `images:` if you reconcile with your own controller.

## 6. Verify

`kubectl get pods -n navigator` should show `navigator-web` running. Hit `https://www.your-domain.example/app/health`
(must return `OK`) and `https://www.your-domain.example/` (must render the home page). The first inbound request starts
embedded Rego, OIDC, and Restate handshakes. Any missing env var crashes the pod with a structured
`enforce_deployment_invariants` error before serving traffic, which is the loud-failure-by-design behavior.

## Where things go from here

- For Restate Cloud setup, see [`gke-prod.md`](gke-prod.md). For driving Navigator from an agent, see
  [`claude-mcp-client.md`](claude-mcp-client.md).
- For an OSS-friendly deploy via GitHub Actions, a fork inherits the canonical
  [`.github/workflows/deploy.yml`](../.github/workflows/deploy.yml), which builds every image and publishes it to Neon
  Law's Artifact Registry through Workload Identity. A fork points the publish jobs at its own registry and identity
  (the `navigator-publisher` service account and the `NAVIGATOR_PUBLISHER_WIF_PROVIDER` secret), then points `ops ship`
  at it with `NAVIGATOR_IMAGE_REGISTRY`; see [`gitops.md`](gitops.md#keyless-pushes-to-artifact-registry).
