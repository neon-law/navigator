---
title: "Notation Machine State"
description: "The persisted specification and current state of one Notation machine."
---

Each [Notation](notation.md) has separate questionnaire and workflow machine records. The durable runtime stores the
machine kind, specification, current state, and ephemeral flag while ordering transitions for that Notation.

The table is `notation_machine_state`.

Schema: [`notation_machine_state`](../../store/postgres/schema.sql).
