# PostgreSQL schema transition artifacts

The [ownership matrix](ownership.json) defines each source table's target scope, migration slice and parity scenario.
The [caller inventory](caller-inventory.md) lists candidate Rust callers and source indexes/relations for review. [SQL
tooling](../../docs/postgres-tooling.md) describes the explicit SQLx migration connection and adoption requirements.
[Compatibility evidence](../../docs/postgres-compatibility.md) covers the pinned image, extensions and DBOS recovery.

`migrations/` contains Rails-style, UTC timestamp-ordered PostgreSQL SQL. The canonical `schema.sql` is assembled from
those files and guarded by a Rust test. IDs use `UUID` with PostgreSQL's random `gen_random_uuid()` default; timestamps
use `TIMESTAMPTZ`. The first migration is an intentionally small Firm → Brand → Project vertical slice with the
reference rows those records need. The next migration adds the Brand-owned `post` table using the glossary noun and
keeps the Surreal source row and PostgreSQL target on the same name; its `social_links` object array is stored as JSONB.
A third migration adds `social_handle` and `social_publication`, the two tables a Post needs beside itself: a Brand's
public handle on each provider, and one Post's delivery state on one provider, unique per Post and provider. The
`clause` table is the first born in PostgreSQL with no Surreal original: the shared Clause library's read model, one row
per `code@version`, versioned by append with at most one `is_current` row per code and a database-checked `body_sha256`.
`rules/clauses/library.yaml` stays the source of truth; `store::clauses::seed` upserts it idempotently, and
`navigator-migrate` runs that seed after a complete migration. Its `ownership.json` entry is `target_only`. A later
migration adds the Firm → Brand → Project membership tables `firm_brand`, `person_firm_role`, and `person_project_role`,
named and shaped as their Surreal sources so the tenancy mirror copies them one to one. The catalog migration adds the
shared catalogs `authority`, `glossary_term`, `question`, `notation_package`, `notation_package_template`, and
`notation_rate` in the same one-to-one shape for the catalog mirror. `template_id` stays without a foreign key until
Template is a PostgreSQL table. Another adds the identity-detail tables a Person or Entity holds (`address`,
`credential`, `email_token`, `entity_role`, `person_delegate`, `person_external_identity`, `person_mailbox`), named as
their Surreal sources; `entity_role` spells the relation's `in` and `out` endpoints as `person_id` and `entity_id`.

The Social Account mirror stores its stable Surreal record key and copies the KMS ciphertext, wrapped data key, key
version, and encryption context byte-for-byte. It never decrypts token material. Its Firm, Brand, and connected or
revoking Person references remain explicit foreign keys. A separate Social Account Selection row stores the saved
Brand/provider destination without credentials or publishing authority; disconnecting a Social Account leaves the
selection intact. The Social OAuth Attempt's provider, expiry, consumed state, and PKCE verifier are mirrored as private
integration state; the backfill report never exposes field values.

The source-domain migration adds PostgreSQL columns for the persistent source tables that were still unmapped: authority
uses and citations, matter records, fiduciary and estate records, document and Notation review state, finance and IOLTA
state, OAuth attempts, and operational aggregates. `draft` remains a derived projection and `schema_version` is
recreated by migrations, so neither is imported as application data. Concrete links keep their UUID columns; union links
and native `relationship` endpoints keep the Surreal table tag and record id in JSONB. Source links deliberately have no
foreign-key constraint in this migration: the importer preserves historical orphans exactly and reports source orphans
separately from PostgreSQL targets that have not yet arrived. The backfill's dry-run and completed-run reports fail
closed while either count is nonzero. Timestamp-like `*_at`, `inserted_at`, and `updated_at` strings are parsed into
`TIMESTAMPTZ`; invalid values fail the row rather than being replaced with the current time.

The optional `navigator ops postgres backfill --dry-run --verify-assets` check also accounts for every storage pointer
currently used by the application. Asset rows are downloaded from private document storage and compared against their
recorded byte length and SHA-256. Brand logos/fonts and Blog Video objects are checked in public asset storage. Raw
email messages and Finance Aggregate inputs/reports are checked in private document storage; their source rows do not
retain an expected digest, so this confirms object existence and reported length only. Output contains aggregate counts
and bytes, never object keys or row values.

These files do not replace `store/src/schema/navigator.surql`. The committed `surreal-source-erd.mmd` and
`parity-manifest.json` inventory the complete current source schema. `postgres-erd.mmd` describes the PostgreSQL tables
that currently exist. The schema parity command remains nonzero while any source table, field, seed, or relationship
shape differs. In particular, the JSONB union references preserve their source table tag and identifier, but they do not
yet have the same SQL column shape as typed UUID references; the runtime backfill independently compares their values
and reports unresolved targets.

The seed check reads the existing synthetic YAML seed catalog and compares each seed table, lookup field, and literal
record field to PostgreSQL. It reports unsupported seed files and fields individually; an empty seed file still requires
a target table. This is a contract check, not a PostgreSQL seed loader. Existing Rust seed logic remains the source of
seed behavior until a separate parity test executes equivalent synthetic fixtures against both stores.

Run the tools from the repository root:

```sh
cargo run -p store --example postgres_schema -- check
cargo run -p store --example postgres_schema -- source-erd
cargo run -p store --example postgres_schema -- erd
cargo run -p store --example postgres_schema -- manifest
```

Each generator rewrites only its named artifact and produces stable bytes. Review both the SQL and regenerated artifacts
in the same change. The tool does not connect to a database, apply migrations, create extensions, or rewrite source
data; [`navigator-migrate`](../../docs/postgres-tooling.md#the-operator-migration-command) applies migrations.

## Temporary journal import at startup

When `NAVIGATOR_POSTGRES_HOST` is configured, the web process imports legacy `journal_entry` rows before accepting
requests. Apply the migrations first with `navigator-migrate`: startup executes no DDL. The import reads SurrealDB
without changing or deleting source rows and inserts Journal Entries, postings, and source references into PostgreSQL in
one transaction. New Journal Entries are written only to PostgreSQL.

Every boot repeats the import. Identical IDs and payloads are skipped; conflicting IDs or command keys fail startup
without overwriting PostgreSQL or committing a partial batch. Reversal targets are imported before their reversals.
Original occurrence times, link verification times, and every other timestamp in the payload remain unchanged. Only rows
with a valid source insertion timestamp migrate. Missing or invalid insertion timestamps are quarantined in the
unchanged source for operator triage; dependent reversals are also skipped. No insertion time is inferred from an
occurrence time or boot time. `legacy_imported_at` records when a timestamped row was imported, and
`legacy_inserted_at_known` is true for every imported row. `legacy_inserted_at_source` retains the original timestamp
text, including precision beyond PostgreSQL’s microsecond timestamp resolution.

The read-only per-Firm triage report exposes only entry and Firm IDs, reason, occurrence time, and the known insertion
timestamp. Startup logs the import and triage counts. Each source read is bounded to 1000 rows; a larger source requires
an explicit reviewed migration rather than an unbounded boot import. Conflicting timestamped entries still abort the
whole transaction, while unresolved reversals are triaged and independent entries can migrate.

This temporary bridge covers the journal only. Firm, Project, Document, authentication, and other application stores
continue to use their existing accessors. Their PostgreSQL parity inventory remains incomplete. Remove the startup
bridge after the deployment transition and source verification; retain its provenance columns for the imported rows.
