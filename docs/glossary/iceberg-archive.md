---
title: "Iceberg Archive"
description: "An Iceberg Archive is a Data Export with database coverage, source watermarks, and Iceberg metadata."
---

An Iceberg Archive holds persistent PostgreSQL rows as Parquet with Iceberg metadata. A reviewed coverage manifest names
every stored relation in each source database. Ordinary views require an explicit reconstruction reason. Stored tables,
materialized views, sequences, DBOS records, AGE catalogs and graph relations, and large-object storage require export.
Added, dropped, inaccessible, unsupported, or unclassified relations fail the generation.

Navigator and Forgejo have distinct source identities and independent extraction watermarks. The paired manifest is
published after both database exports and their Iceberg metadata succeed. Its `cross_database_atomic` field is false:
paired publication proves coverage, while a shared transactional recovery point requires a tested coordination
procedure. Sequence values are outside PostgreSQL's MVCC snapshot and are listed separately for source-specific recovery
checks.

Rehydration applies each database's own schema procedure into an isolated target, checks object hashes, reads Iceberg
files, restores rows using source column types, and verifies restored rows. AGE schema reconstruction resolves target
catalog identifiers through its native procedures. A successful row restore alone does not establish DBOS worker replay
or Forgejo service recovery.

Git repositories, LFS, attachments, and document-object bytes have separate backup and matching recovery-point
contracts. Physical PostgreSQL backups and WAL provide point-in-time recovery. An Iceberg Archive does not replace those
contracts.

- [Data Export](data-export.md)
- [Archive coverage implementation](../../archives/src/coverage.rs)
