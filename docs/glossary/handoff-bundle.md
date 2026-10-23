---
title: "Handoff Bundle"
description: >-
  A Handoff Bundle is the verified set of current legal files one recipient may receive from a Project, with an
  immutable checksum manifest.
---

What a recipient — the client, successor counsel, or another participant — takes away from a [Project](project.md). It
holds the current revision of every live [Document](document.md) the recipient's own matter lens admits, and nothing
else: no source or history (a [Closed Repository](closed-repository.md)), no deleted chains, no Notation internals, no
credentials, and none of a [Project Recovery Archive](project-recovery-archive.md)'s restore metadata.

Preparing a handoff reads every file from Navigator storage — the active copy, or the verified archived copy once the
active one is gone — and checks it against its recorded size and SHA-256 before an immutable manifest of entry names,
revision ids, sizes, and checksums is recorded. A missing or damaged file fails the handoff. Each download rebuilds the
ZIP from that manifest, `MANIFEST.json` first, and verifies every byte again. A handoff marked required is one the
Project's archival waits on until the recipient acknowledges that exact manifest digest; a retried acknowledgment
converges and a different digest is refused.

- Service: [`store::project_handoffs`](../../store/src/project_handoffs.rs)
- Table: `project_handoff` in [`store/postgres/schema.sql`](../../store/postgres/schema.sql)
