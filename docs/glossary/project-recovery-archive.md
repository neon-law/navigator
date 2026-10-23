---
title: "Project Recovery Archive"
description: >-
  A Project Recovery Archive is a verified, immutable generation of one Project's complete recoverable record, kept in
  the private archived-projects bucket.
---

The internal copy that lets a [Project](project.md) be rebuilt after its active copies are gone. Each generation holds
every [Revision](revision.md) of every [Document](document.md) on the Project (soft-deleted ones included; a purged one
keeps only its tombstone row), the [Notation](../notation.md#notation) source and rendered output, the Project-scoped
rows a restore writes back, and a verified reference to the Project repository's Git bundle backup. It never holds
credentials, another Project's or Firm's records, or the durable-execution engine's internal state.

A generation is copied to content-addressed objects, read back, and checked by size and SHA-256 one item at a time. It
commits only when every expected item verifies and its manifest has been written and read back; a committed generation
never changes. A restore returns every identity, revision, checksum, provenance link, visibility, [Soft
Deletion](soft-deletion.md) stamp, and [Legal Hold](legal-hold.md) as archived, and never overwrites a different row.
Requesting, reading, and restoring a generation are Firm-scoped: they need the Firm's recovery-archive capability.

Three neighbours are different things. A [Closed Repository](closed-repository.md) is a working-tree ZIP of the
repository's final commit, filed as a document. A handoff bundle is the legal files a recipient is authorized to
receive, without source, history, or internal state. Full-service disaster recovery restores whole databases and
buckets, not one Project.

- Service: [`store::project_archives`](../../store/src/project_archives.rs)
- Tables: `project_archive_generation` and `project_archive_item` in
  [`store/postgres/schema.sql`](../../store/postgres/schema.sql)
- Operations: [Project recovery archives](../project-recovery-archives.md)
