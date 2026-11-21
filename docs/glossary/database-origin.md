---
title: "Database Origin"
description: "A Database Origin records how an application database acquired initial authority."
---

A Database Origin records how an application database acquired initial authority. A fresh-native origin binds an
actually newly created database to its creation operation, verified tier, and initial authority and schema contracts. It
is distinct from captured-source enrollment and durable-object ownership.

Only a successful unique database creation can initialize a fresh-native origin, before application data is seeded. An
empty database, a runtime flag, or an existing connection cannot establish that origin. Unknown, foreign, partial, or
ambiguous authority fails admission. Restoring rows into another database does not transfer its creation identity.

The table is `database_origin`.

Its effective schema is resolved through completed records in `database_origin_upgrade`.

A schema upgrade records its bound intent before migration. Pending upgrades block application admission; only the same
operation can resume a verified checksum prefix and record completion. Creation evidence stays immutable. A changed
authority registry requires a separately reviewed transition and is not inferred from schema emptiness.

The table is `database_origin_upgrade`.

Schema: [`database_origin`](../../store/postgres/schema.sql) and
[`database_origin_upgrade`](../../store/postgres/schema.sql).

A pending [Database Recovery](database-recovery.md) retains its own creation identity and blocks admission. Unverified
archive input cannot establish recovery completion or transfer source authority.
