# PostgreSQL schema

`migrations/` contains timestamped PostgreSQL SQL. The schema tooling assembles `schema.sql` in migration order and
verifies the committed schema and entity relationship diagram. IDs use `UUID` and timestamps use `TIMESTAMPTZ`.

Run these commands from the repository root.

```sh
cargo run -p store --example postgres_schema -- check
cargo run -p store --example postgres_schema -- erd
```

Apply migrations with [`navigator-migrate`](../../docs/postgres-tooling.md#the-operator-migration-command). [Storage
models and writes](../../docs/storage-models.md) covers application admission and enrollment receipts.
