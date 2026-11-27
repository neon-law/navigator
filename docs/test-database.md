# PostgreSQL test fixtures

Task-owned KIND teardown removes only its own resources. Repeated startup verifies the same database and durable history
rather than truncating it; retained or partially initialized history requires explicit recovery.

## The PostgreSQL fixture

PostgreSQL is the storage authority, so tests exercise it directly. The fixture is one disposable container of the
pinned `navigator/postgres` image on loopback port 25432, with `store/postgres/schema.sql` applied. Tests read the port
from `NAVIGATOR_TEST_POSTGRES_PORT`; every PostgreSQL-backed test fails when the port is missing, through
`store::test_support::postgres_test_port`. Start the fixture locally the way CI does (the `start PostgreSQL for covered
journal tests` step in `.github/workflows/ci.yml`):

```bash
docker run -d --name test-postgres --shm-size=256m -p 127.0.0.1:25432:5432 \
  -e POSTGRES_PASSWORD=synthetic -e POSTGRES_DB=smoke \
  ghcr.io/neon-law/navigator/postgres@sha256:977ff28cdc30648612b50c7a25963670b867f9aa03d361db1baf45a64e01621b
docker exec -i test-postgres psql -U postgres -d smoke -v ON_ERROR_STOP=1 < store/postgres/schema.sql
```

The shared unit-test server hosts isolated test databases. `PostgresFixture::create` uses the native creation capability
and embedded migrations to record a fresh [Database Origin](glossary/database-origin.md) before any data. It writes no
captured-import completion receipts. Its consuming cleanup drops only that owned resource. Explicit capture-target and
unactivated fixtures retain their separate admission contracts. These fixtures do not certify Kubernetes role
boundaries, production cutover, or recovery.

### DBOS workflow tests

Set `NAVIGATOR_TEST_POSTGRES_PORT` to the test server port. Run the entire DBOS application test binary with:

```sh
cargo nextest run --locked -p workflows-service --test dbos
```

Each case owns its database. Tests that launch application work in new tasks run the worker in a child process with that
database in its connection settings. The PostgreSQL image smoke test also kills and restarts the standalone DBOS worker
between a business write and its checkpoint.

## Fresh KIND application tiers

The fresh local KIND tier is distinct from the focused unit-test server above. Hudson creates a unique PostgreSQL
database and records its [Database Origin](glossary/database-origin.md) before seeding through admitted accessors. It
provisions TLS and application, migration, DBOS, and archive roles. The web process hosts DBOS workflows.

```bash
cargo run -p hudson -- dev worktree-env up --runtime kind --path "$PWD"
set -a; source .devx/env; set +a
```

Build the Dioxus client from the same checkout as the web server before running a hydrated browser test. The command
stages the generated client in the ignored `server/public/dioxus` directory.

```bash
cargo run -p hudson -- dev build-webapp
cargo run -p neon
```

Private `.devx/postgres/env` and `.devx/env` bind the application and host helpers to that exact database. The CA stays
in `.devx/postgres/ca.crt`; server keys remain with PostgreSQL. The owned KIND node exposes the TLS NodePort on
loopback, while its verified node-facing ingress rule and database credentials remain required. Repeated starts verify
the namespace and database identity. Unknown or partial authority is never relabeled as a fresh database.

The full native development harness uses existing capturing providers only under its guarded embedded Dev, CI, and
simulated-matter configuration. Those captures neither send external effects nor certify live-provider behavior. A
successful fixture or local startup is not retained-work handover or recovery acceptance.

## Native deploy browser gate

The `.github/workflows/deploy.yml` integration job provisions its native tier through the ordinary quickstart CLI, then
verifies the exact built application image as nonroot. It requires native admission, persisted DBOS workflow replay
across host/image restart, and all three hydrated browser suites. Its independent practice previews use owned PostgreSQL
fixtures. Use the PostgreSQL fixture for focused tests. The KIND gate covers the exact application image, browser
checks, and durable checkpoints through restart.

## Run tests

Run the workspace suite against the disposable PostgreSQL server.

```sh
cargo nextest run --workspace
```

A package-filtered `webapp` run needs the crate's `server` feature. A workspace nextest invocation already compiles
`webapp` through dependents that enable it; running the crate alone does not:

```sh
cargo nextest run -p webapp --features server
```

## CI

The Rust coverage job provisions PostgreSQL and sets `NAVIGATOR_TEST_POSTGRES_PORT`. The KIND gate exercises the browser
and accessibility suites. Marking a draft pull request ready starts the Rust suite.
