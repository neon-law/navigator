---
name: surreal-migrations
description: >
  Change the SurrealDB schema safely: add, retype, or remove a field in `store/src/schema/navigator.surql`, bump
  `SCHEMA_VERSION`, and ship the backfill that the applied-schema model cannot perform for you. Trigger when the user
  says "add a field", "change the schema", "write a migration", "backfill", "retype a column", or when a change edits
  `navigator.surql` or a row struct that reads it. Covers why a green test suite proves nothing about staging and
  production.
---

# Surreal migrations

**SurrealDB is being replaced by PostgreSQL.** This skill covers the store that still runs today. Before designing a new
table or a large schema change, read the [`postgres`](../postgres/SKILL.md) skill and prefer a shape that ports cleanly
to relational tables (nouns become tables, per the glossary).

Before beginning, always ground in the code, its tests, and the [glossary](../../../docs/glossary).

Read the module docs in [`store/src/schema/mod.rs`](../../../store/src/schema/mod.rs) and the "Production database"
section of [`docs/cloud-operations.md`](../../../docs/cloud-operations.md) before acting. The schema states the present;
it is not a chain of steps, so any change to existing data is your job.

## What applying the schema does and does not do

[`navigator.surql`](../../../store/src/schema/navigator.surql) is one idempotent file describing the tables and fields
that should exist. `store::schema::apply` runs it whole on every boot and in every test that opens an embedded engine,
then records `SCHEMA_VERSION`. Bump that constant in the same change that edits the file, or a database prepared by
another build reports as in sync while disagreeing.

Applying converges *definitions*. It never touches an existing row:

- `DEFINE TABLE IF NOT EXISTS` deliberately leaves an existing table's rows alone.
- `DEFAULT` on a `DEFINE FIELD` is a **write-time** default. It supplies a value for rows written after the definition
  lands. It does not reach back into rows written before it, which keep no value for that field at all.

So adding a required field is two changes, not one: the definition, and a backfill for the rows that predate it.
Backfills stay explicit one-shot jobs.

**A green workspace proves nothing here.** Every test opens a fresh embedded engine, so every row it reads was written
by the current schema and carries every field. The only databases that hold pre-field rows are staging and production,
and nothing in CI resembles them.

## Adding or retyping a field

1. **Define it.** Edit `navigator.surql`. Prefer `option<T>` for anything a historical row may lack. Reach for a
   non-optional `T` with `DEFAULT` only when you are also shipping step 3.
2. **Bump `SCHEMA_VERSION`** in [`store/src/schema/mod.rs`](../../../store/src/schema/mod.rs), same change.
3. **Make the read tolerate a missing value, or backfill before the read ships.** A row struct field typed bare `T`
   fails to read a row that lacks it (`Expected bool, got none`), and on a boot path that crash-loops the binary. Type
   the field `Option<T>` and collapse it in the `into_*` conversion (`.unwrap_or_default()`); this survives a backfill
   that has not run yet. A backfill alone leaves reads failing between deploy and job.
4. **Cover the historical row.** Write a test that creates a record without the new field (raw `CREATE` or `UPDATE
   ... UNSET`), then reads it through the same accessor production uses. Without it, the suite only sees rows the
   current schema wrote.
5. **Write the backfill.** Idempotent and guarded on the old value, so re-running it is safe and it cannot overwrite a
   real value:

   ```surql
   UPDATE person SET email_confirmed = false WHERE email_confirmed IS NONE;
   ```

6. **Run it per row, staging first.** Follow `docs/cloud-operations.md` § "Production database": write the exact
   SurrealQL to a timestamped file under `/tmp/navigator-prod-sql/`, show the user the path and contents, wait for
   approval of that exact statement, then verify the row count afterwards. Read-only `SELECT`s need no ceremony; every
   `UPDATE`, `DELETE`, and `DEFINE` does.

## Removing a field

Stop reading it first, in a released build, then drop the definition in a later change. A definition removed while a
deployed binary still selects the column turns every read into a failure with no rollback path except redeploying the
old tag.

## Checklist

- `navigator.surql` edited and `SCHEMA_VERSION` bumped in the same commit.
- Every new non-optional field has either a tolerant reader or a backfill that ships first.
- A test covers a row written without the field.
- The backfill is idempotent, guarded, and recorded where the operator running it will find it.
- Staging ran the backfill and served the new binary before production saw either.
