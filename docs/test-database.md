# Test database — PostgreSQL fixture first, embedded SurrealDB deprecated

> **New tests run against the PostgreSQL fixture.** The embedded SurrealDB harness is deprecated: its call sites only go
> down, and `cli/tests/embedded_surreal_ratchet.rs` fails a change that adds one.

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
  ghcr.io/neon-law/navigator/postgres@sha256:e9e013fb71f4f6f999debedbda22f3163e80d7e99a028c47b4609a69ddb096de
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
provisions TLS and application, migration, DBOS, and archive roles. `web` cohosts the selected native workflows; fresh
dependencies contain no SurrealDB server, Restate broker, or separate workflow worker.

```bash
cargo run -p hudson -- dev worktree-env up --runtime kind --path "$PWD"
set -a; source .devx/env; set +a
```

Private `.devx/postgres/env` and `.devx/env` bind the application and host helpers to that exact database. The CA stays
in `.devx/postgres/ca.crt`; server keys remain with PostgreSQL. The owned KIND node exposes the TLS NodePort on
loopback, while its verified node-facing ingress rule and database credentials remain required. Repeated starts verify
the namespace and database identity. Unknown or partial authority is never relabeled as a fresh database.

The full native development harness uses existing capturing providers only under its guarded embedded Dev, CI, and
simulated-matter configuration. Those captures neither send external effects nor certify live-provider behavior. A
successful fixture or local startup is not retained-work handover or recovery acceptance.

## Captured compatibility

Existing legacy KIND descriptors retain captured-source enrollment. Explicit `--runtime native` selects the separate
host-process compatibility topology, not fresh PostgreSQL authority. Compatibility bootstrap holds web and worker
writers, captures the existing Source export, and enrolls required scopes before sample seeding. First enrollment
requires the source tables to exist with only fixture metadata populated. Startup leaves source definitions and rows
untouched. A valid PostgreSQL enrollment can be reused after the source fixture loses its in-memory schema.

## Native deploy browser gate

The `.github/workflows/deploy.yml` integration job provisions its native tier through the ordinary quickstart CLI, then
verifies the exact built application image as nonroot. It requires native admission, persisted DBOS workflow replay
across host/image restart, and all three hydrated browser suites. Its independent practice previews use owned PostgreSQL
fixtures. It does not provision Source, a Restate broker, or a separate workflow worker.

The explicit compatibility suites retain their own fixtures. Passing a focused native test does not establish that the
full image, browser, recovery, or cold-runner resource gates passed.

## The embedded SurrealDB harness (deprecated)

`store::test_support::mem_surreal()` (and `store::surreal::test_support::mem()`, and `features::shared_surreal()`)
starts an embedded `kv-mem` SurrealDB engine inside the test process. It costs about a second per test, it is most of
the workspace suite's time, and the embedded SurrealDB 4 engine intermittently hangs. The CI nextest profile kills any
test after five minutes so its retries run it again.

The embedded engine is an explicit build capability, separate from `store/test-support`. Primary CI keeps its
Surreal-free test selection and does not enable `store/surreal-compatibility`. An unfiltered workspace run includes
embedded compatibility tests, so request that capability explicitly:

```bash
cargo test --workspace \
  --features store/surreal-compatibility,workflows-service/jwt-provider-regression
```

The JWT regression feature tests an unpinned two-provider process separately from startup pinning before the first
health probe. Provider selection lasts for the process lifetime; pinning cannot repair a process that already selected
an ambiguous provider. The compatibility run needs its documented PostgreSQL and Source fixtures. The embedded engine
feature does not disable `NAVIGATOR_FORBID_SURREAL`: a primary-lane environment still refuses an attempted engine start.
Without the feature, calling an embedded fixture fails rather than silently skipping it. Remote HTTP/WebSocket
compatibility clients remain available without the embedded engine.

The existing call sites move to the PostgreSQL fixture crate by crate as each domain's PostgreSQL authority lands
(ENG-1356). The ratchet holds each crate's count: a port that removes call sites lowers that crate's `BASELINE` entry in
the same change.

## Explicit Source compatibility subprocess tests

A Source compatibility subprocess needs a real Source server rather than an in-process engine.
`store::test_support::server_surreal(database)` supplies that explicit lane with a separate named database. Native
subprocess tests instead use their actual PostgreSQL target and normal admission; they do not need a Source engine.

- **`NAVIGATOR_SURREAL_ENDPOINT` set** → connect, apply the schema, run.
- **unset** → return `None`, and the test skips.

`NAV_REQUIRE_SURREAL=1` turns the compatibility skip into a failure. A half-configured Source environment is an error,
not an opt-out. The primary coverage lane selects tests without Source using the canonical test inventory.

## Test tiers

- **Focused native tests:** disposable PostgreSQL fixtures and existing provider seams; no application sidecars are
  needed merely to test a store accessor.
- **Fresh KIND and quickstart:** the real PostgreSQL/DBOS application topology with Rauthy, storage, and owned services.
- **Native deploy browser gate:** exact application image, native admission, workflow checkpoint replay, and hydrated
  browser checks.
- **Compatibility suites:** retained Source/Restate fixtures remain explicit. Full-workspace
  commands can include them; do not confuse them with the Source-free coverage selection.

Do not add application sidecars to focused native tests; use their existing provider seams.

## How a contributor runs tests

The full compatibility-inclusive workspace command is:

```sh
cargo nextest run --workspace \
  --features store/surreal-compatibility,workflows-service/jwt-provider-regression
cargo test -p features --features store/surreal-compatibility
```

Provide the PostgreSQL fixture when running the full workspace. The Cucumber BDD suites in `features` use `cargo test`
because their custom harness uses a different test protocol.

A package-filtered `webapp` run needs the crate's `server` feature. A workspace nextest invocation already compiles
`webapp` through dependents that enable it; running the crate alone does not:

```sh
cargo nextest run -p webapp --features server
```

To also exercise the server-mode lane — the WebSocket protocol, root `signin`, and namespace/database selection over a
wire, which an in-process engine cannot cover — point the harness at a running engine:

```sh
set -a; source .devx/env; set +a
NAV_REQUIRE_SURREAL=1 cargo nextest run --workspace \
  --features store/surreal-compatibility,workflows-service/jwt-provider-regression
```

## How CI runs them

The primary Rust coverage job provisions PostgreSQL and sets `NAVIGATOR_TEST_POSTGRES_PORT` and
`NAVIGATOR_FORBID_SURREAL=1`. `hudson ops surreal-free-tests` filters out engine-backed test modules, integration
binaries, and Cucumber runners. Integration tests that use `with_required_postgres` and reach no Source engine stay in
the coverage measurement, including tests that pass a disconnected Source handle through application APIs. The KIND gate
exercises the browser and accessibility suites. Marking a draft pull request ready starts the Rust suite.
