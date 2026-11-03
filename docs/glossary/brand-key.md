---
title: "Brand Key"
description: "A Brand Key is the short machine name that identifies one Brand across the whole deployment."
---

The machine name of one [Brand](brand.md): `brand.brand_key`, such as `neon` or `delete-your-data`. **The `brand` table
is the source of truth**; every other place a key appears refers back to that row: `firm_brand.brand_key`,
`project.brand`, a Project's `navigator.yaml` `project.brand`, the `--brand` option on `navigator project create`,
`scaffold`, and `set`, and the `{key}` segment of `/app/admin/brands/{key}/edit`. A [House Brand](house-brand.md) is the
compiled routing key with the same spelling.

**Format.** By convention a key is lowercase ASCII letters and digits, words joined by single hyphens, with no spaces,
underscores, or slashes (`death-and-divorce`). Every House Brand follows it, and a new key should too. The create form
at `/app/admin/brands/new` does not yet enforce the format, so a stored key is compared exactly as typed: matching is
case-sensitive, `Neon` is not `neon`, and a key holding a `/` cannot be reached at its edit path. The display `name`
(`Delete Your Data`) is a separate field; a command that takes a key never accepts the name.

**Unique across the deployment.** A key names at most one Brand (`brand.brand_key UNIQUE` in PostgreSQL,
`brand_key_unique` in SurrealDB) and is worn by at most one Firm (`firm_brand.brand_key UNIQUE`, `firm_brand_key` in
SurrealDB). **A Brand Key therefore implies its Firm**: the Firm wearing it is the one [Firm Brand](firm-brand.md) row
that names it. `navigator project set --firm <name> --brand <key>` names which rule refused a pair: a key no Brand
carries, a key no Firm wears, or a key a different Firm wears.

**Finding a key.** An operator reads it from:

- `GET /app/api/firms`, whose `brands` array lists the keys each visible Firm wears;
- the Edit link on each card at `/app/admin/brands`, whose path carries the key;
- the edit page itself, `/app/admin/brands/{key}/edit`, which answers 404 both for a key no Brand carries and for a
  Brand the caller may not manage, so the page never discloses another Firm's row.

The column is `brand.brand_key`.

Schema: [`brand`](../../store/postgres/schema.sql).
