---
title: "Notation Transition"
description: "A Notation Transition retains one runtime operation until its state and effects finish."
---

A Notation Transition owns one [Notation](notation.md) across its questionnaire and workflow machines until that
operation completes. The target-only PostgreSQL `notation_transition` table records the logical operation UUID,
immutable input fingerprint, and completed result. An unfinished row prevents another operation on that Notation.
Ephemeral runtime fixtures may lack a Notation row; these records grant no matter visibility or participation.

The first DBOS checkpoint supplies the logical UUID. A second pure anchor commits before admission so a fork from the
last recorded step retains the UUID when the failure occurs outside a step. Recovery and repair forks reuse it. An
arbitrary fork before that checkpoint is a different operation and cannot bypass an existing pending owner. Validation
before any checkpointed machine read or effect can discard its unstarted claim; later failures retain ownership.

Completion records the result and releases pending ownership in one database update. This orders distinct runtime
operations; an external effect can still repeat if its send succeeds before the DBOS checkpoint commits.

The table is `notation_transition`.
