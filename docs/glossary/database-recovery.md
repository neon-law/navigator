---
title: "Database Recovery"
description: "A Database Recovery fences a newly created resource while archive data is restored."
---

A Database Recovery binds a logical recovery attempt to a newly created database and retains the exact archive manifest
and source identity bytes. These bytes are unverified input, not an activation receipt. The target keeps its own
immutable [Database Origin](database-origin.md); restoring source rows never transfers creation identity.

The table is `database_recovery`. Its pending intent blocks application admission before restored rows can be written.
It creates no fresh durable-object epoch. Pending input cannot fall back to captured-source authority.

Completion independently parses the bound archive, validates source migration lineage, and verifies exact target schema,
relation closure, stored rows and sequence safety behind a connection fence. It records completion atomically without
changing target Origin or archived source evidence. Only completed verification admits the restored resource.
Verification refusals after sealing keep connections closed. An unconfirmed commit or reopen requires explicit
inspection rather than automatic replay.

The archive preserves separate per-database recovery points. Sequence capture is outside MVCC. An external byte pointer
is not recovery acceptance for its document, Git or LFS payload. Completion does not establish Forgejo service boot,
external provider effects, Kubernetes runtime-role acceptance or a shared recovery point. Physical base backup and WAL
remain required.
