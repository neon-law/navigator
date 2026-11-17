# Test database — PostgreSQL fixture first, embedded SurrealDB deprecated

> **New tests run against the PostgreSQL fixture.** The embedded SurrealDB harness is deprecated: its call sites only go
> down, and `cli/tests/embedded_surreal_ratchet.rs` fails a change that adds one.

The local KIND lifecycle resets staging by deleting its guarded namespace, never by truncating tables; old workflow
journals cannot survive.

## The PostgreSQL fixture

PostgreSQL is the storage authority, so tests exercise it directly. The fixture is one disposable container of the
pinned `navigator/postgres` image on loopback port 25432, with `store/postgres/schema.sql` applied. Tests read the port
from `NAVIGATOR_TEST_POSTGRES_PORT` and self-skip without it. `NAV_REQUIRE_POSTGRES=1` turns that skip into a failure.
Start the fixture locally the way CI does (the `start PostgreSQL for covered journal tests` step in
`.github/workflows/ci.yml`):

```bash
docker run -d --name test-postgres --shm-size=256m -p 127.0.0.1:25432:5432 \
  -e POSTGRES_PASSWORD=synthetic -e POSTGRES_DB=smoke \
  ghcr.io/neon-law/navigator/postgres@sha256:a3f6db0888d3d68c0ccadf936784768ecda4131b58fba30d631d69ff58337a8e
docker exec -i test-postgres psql -U postgres -d smoke -v ON_ERROR_STOP=1 < store/postgres/schema.sql
```

## The embedded SurrealDB harness (deprecated)

`store::test_support::mem_surreal()` (and `store::surreal::test_support::mem()`, and `features::shared_surreal()`)
starts an embedded `kv-mem` SurrealDB engine inside the test process. It costs about a second per test, it is most of
the workspace suite's time, and the embedded SurrealDB 4 engine intermittently hangs. The CI nextest profile kills any
test after five minutes so its retries run it again.

The existing call sites move to the PostgreSQL fixture crate by crate as each domain's PostgreSQL authority lands
(ENG-1356). The ratchet holds each crate's count: a port that removes call sites lowers that crate's `BASELINE` entry in
the same change.

## The one exception — a test that spawns `navigator`

A subprocess cannot reach an in-process engine, so a test that runs the binary needs a real server instead.
`store::test_support::server_surreal(database)` connects to one and hands back the coordinates to point that process at,
on a database of the caller's own naming so two such tests cannot collide.

- **`NAVIGATOR_SURREAL_ENDPOINT` set** → connect, apply the schema, run.
- **unset** → return `None`, and the test skips.

`NAV_REQUIRE_SURREAL=1` turns that skip into a failure, which is what keeps CI honest. A half-configured environment (an
endpoint with no namespace) always panics: that is a mistake, not an opt-out.

## Test tiers

- **Tier 1 — `cargo test` and BDD:** the PostgreSQL fixture, plus the embedded store where it remains; embedded Rego,
  Rauthy, Restate, storage, vendors, and agent routing use their in-process test seams.
- **Tier 2 — KIND e2e:** Rauthy, embedded Rego, Restate, SeaweedFS, browser, and accessibility use the real topology.

Do not pull other sidecars into tier 1. Its trait seams are the contract boundary.

## How a contributor runs tests

```sh
cargo nextest run --workspace && cargo test -p features
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
NAV_REQUIRE_SURREAL=1 cargo nextest run --workspace
```

## How CI runs them

CI does not test SurrealDB. The test job starts the PostgreSQL fixture and sets `NAVIGATOR_TEST_POSTGRES_PORT`.
`scripts/surreal-free-tests.py` leaves out every test in a file that starts a SurrealDB engine, and every cucumber
runner that reaches one, and leaves every file that names SurrealDB out of the coverage measurement. The job sets
`NAVIGATOR_FORBID_SURREAL=1`, so a test that starts an engine anyway fails instead of running. KIND e2e covers the real
stack, browser, and accessibility. Draft pull requests skip the Rust suite; marking one ready runs it.
