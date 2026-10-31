---
title: "Matter Note"
description: >-
  A Matter Note is a firm-internal observation a lawyer files on a matter, rendered on the workbench's Notes tab.
---

A firm-internal observation a lawyer files on a [Matter](matter.md) — the substance the Notes tab of the matter show
renders. A matter note is a [Communication](communication.md) on the `note` channel: it rides the same privileged
conversation spine as email and logged calls, with no counterparty, no external source identifier, and no client
visibility. Its direction is `internal` by construction, so the client view's exclusion is structural — the client-side
query filters internal rows rather than trusting each renderer to hide them.

`navigator project communication note` files one (`--body-file`, optional `--subject`); the same door serves the
workbench at `POST /app/api/projects/{id}/communications`. Communications stay server-only: a Project repository carries
no pointer for a note.

- Schema and queries: [`store::communications`](../../store/src/communications.rs)
