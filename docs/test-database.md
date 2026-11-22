# Test database — PostgreSQL fixture first, embedded SurrealDB deprecated

> **New tests run against the PostgreSQL fixture.** The embedded SurrealDB harness is deprecated: its call sites only go
> down, and `cli/tests/embedded_surreal_ratchet.rs` fails a change that adds one.

Task-owned KIND teardown removes only its own resources. Repeated startup verifies the same database and durable history
rather than truncating it; retained or partially initialized history requires explicit recovery.

## The PostgreSQL fixture

PostgreSQL is the storage authority, so tests exercise it directly. The fixture is one disposable container of the
pinned `navigator/postgres` image on loopback port 25432, with `store/postgres/schema.sql` applied. Tests read the port
from `NAVIGATOR_TEST_POSTGRES_PORT` and self-skip without it. `NAV_REQUIRE_POSTGRES=1` turns that skip into a failure.
Start the fixture locally the way CI does (the `start PostgreSQL for covered journal tests` step in
`.github/workflows/ci.yml`):

```bash
docker run -d --name test-postgres --shm-size=256m -p 127.0.0.1:25432:5432 \
  -e POSTGRES_PASSWORD=synthetic -e POSTGRES_DB=smoke \
  ghcr.io/neon-law/navigator/postgres@sha256:1978da25d0d797ebdd92a6fac7085921940f0e69b7db78c0f2f2473a65ee214c
docker exec -i test-postgres psql -U postgres -d smoke -v ON_ERROR_STOP=1 < store/postgres/schema.sql
```

The shared unit-test server hosts isolated test databases. `PostgresFixture::create` uses the native creation capability
and embedded migrations to record a fresh [Database Origin](glossary/database-origin.md) before any data. It writes no
captured-import completion receipts. Its consuming cleanup drops only that owned resource. Explicit capture-target and
unactivated fixtures retain their separate admission contracts. These fixtures do not certify Kubernetes role
boundaries, production cutover, or recovery.

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

## Captured compatibility and the deploy browser gate

Existing legacy KIND descriptors retain captured-source enrollment. Explicit `--runtime native` selects the separate
host-process compatibility topology, not fresh PostgreSQL authority. Compatibility bootstrap holds web and worker
writers, captures the actual Source export, and enrolls required scopes before sample seeding. Valid enrollment is
reused; unactivated nonempty or partial history requires explicit recovery rather than invented receipts.

The existing `.github/workflows/deploy.yml` browser job remains a legacy compatibility gate. It installs the Restate
operator and CRDs, invokes `hudson dev postgres-bootstrap`, and forwards the in-cluster Source service. Its
PracticePreview helper also retains an embedded-engine fixture. This gate is separate from fresh local provisioning and
the dedicated quickstart job; neither establishes that all CI runs without legacy engines.

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
- **Compatibility suites and deploy browser gate:** retained Source/Restate fixtures remain explicit. Full-workspace
  commands can include them; do not confuse them with the Source-free coverage selection.

Do not add application sidecars to focused native tests; use their existing provider seams.

## How a contributor runs tests

The full compatibility-inclusive workspace command is:

```sh
cargo nextest run --workspace \
  --features store/surreal-compatibility,workflows-service/jwt-provider-regression
cargo test -p features --features store/surreal-compatibility
```

Without the PostgreSQL fixture running, its tests skip. The cucumber BDD suites in `features` keep `cargo test`; their
custom harness does not speak nextest's protocol, so the workspace nextest profile excludes that package.

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

The primary Rust coverage job does not start SurrealDB. It starts the PostgreSQL fixture and sets
`NAVIGATOR_TEST_POSTGRES_PORT`. `hudson ops surreal-free-tests` leaves out every test in a file that starts a SurrealDB
engine, every test binary and cucumber runner that reaches one, and leaves every file that names SurrealDB out of the
coverage measurement. The job sets `NAVIGATOR_FORBID_SURREAL=1`, so a test that starts an engine anyway fails instead of
running. KIND e2e covers the real stack, browser, and accessibility. Draft pull requests skip the Rust suite; marking
one ready runs it.
