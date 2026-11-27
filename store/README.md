# Store

`store` owns Navigator's PostgreSQL schema, canonical seeds, connection configuration, and shared data-access rules. The
[canonical schema](postgres/schema.sql) is assembled from ordered [migrations](postgres/migrations/).

Role determines an authenticated person's application tier; Project participation determines matter scope. See the
[glossary](../docs/glossary/README.md), [access model](../docs/access-model.md), and [test
database](../docs/test-database.md).
