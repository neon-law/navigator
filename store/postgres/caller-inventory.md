# Source caller inventory

Run `cargo run -p store --example postgres_callers -- write` after changing a store module, query, or Surreal table.
`check` verifies the committed inventory. The generator reads every source table, field, index and relation declaration
from the canonical Surreal schema, notes whether a PostgreSQL table exists, and lists Rust files with a whole-identifier
mention of the table. The list is deterministic.

[Source call sites](source-call-sites.jsonl) records syntax-parsed query/CRUD and transaction calls, with file, line,
enclosing function, operation, SQL operation words and referenced tables. `query_call_ids` indexes those JSONL rows from
zero. Local constant arguments and composed expressions are included. Calls with runtime-only arguments remain in the
source list even when no table can be resolved statically; their shared backend contract must be reviewed when porting
the facade. Query contents and literal values are omitted from this artifact.

The JSON is a **candidate list**. A mention can be a comment, fixture, or unrelated variable. Queries assembled from
fragments require the call-site inventory and their constant definitions. Each candidate needs a human review of its
read and write operations, transaction boundaries, route or workflow entry point, destination owner, migration slice,
and covering parity test before its table is declared ported. The `review_state` field keeps this gap visible in every
row. The [schema parity manifest](parity-manifest.json) and seed check remain the schema and seed evidence; they also do
not establish runtime parity.

## Runtime evidence

[Runtime parity evidence](runtime-parity.json) records each ownership scenario, reviewed test references, the assertion
scope of those tests, and remaining acceptance. The generator includes this evidence in every inventory row. It refuses
missing or extra source-table entries, stale scenario wording, invalid paths, and references that do not identify one
non-ignored async test. Update the evidence when changing a scenario or renaming a referenced test, then regenerate.

References describe reviewed source assertions. They are not test execution results, exhaustive coverage, or permission
to switch reads. A mirror round trip does not establish equivalent application queries or access decisions. Every row
retains explicit remaining acceptance, including rows with PostgreSQL tables and runtime tests. An empty test list means
no reviewed reference is linked here; it does not claim that the repository contains no related tests.

Run the referenced tests with a required synthetic PostgreSQL fixture: set `NAVIGATOR_TEST_POSTGRES_PORT` and
`NAV_REQUIRE_POSTGRES=1`. Otherwise the mirror tests can return early. Most references are store unit tests, selected
with `cargo test -p store --lib <function>`; references under `store/tests/` use that integration-test target instead.

Start with the ownership rules in [postgres-tenancy.md](../../docs/postgres-tenancy.md). [ownership
matrix](ownership.json) maps every source table to a target scope, ownership path, migration slice and parity scenario.
The generator refuses a missing or stale ownership entry. Its destination records derived views and AGE projections
separately from ordinary SQL tables. A target table's presence means only that a table declaration exists, not that
reads or writes use it.
