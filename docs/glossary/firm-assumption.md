---
title: Firm Assumption
description: >-
  A Firm's stated premise the CEO Digest checks actuals against, held as append-only versioned rows so history is the
  table.
---

A **Firm Assumption** is a stated premise a [Firm](firm.md) holds about its own plan work — for example the target
minutes a document review should take. The [CEO Digest](ceo-digest.md) compares actuals against each assumption and
reports one of three verdicts: holding, change the assumption, or change the work. Each assumption is a `key` (a dotted
path such as `plan.target_minutes.document_review`) plus a JSON `value`, `set_by` the person who recorded it, and the
`reason` they gave. A change is a new row with a higher `version`; UPDATE and DELETE are refused by trigger, so history
is the table.

Keys are cataloged in the repository's `assumptions/schema.yaml` with their types and null defaults; the values
themselves are [Firm](firm.md) data in PostgreSQL and never enter the repository. Kits reference keys and resolve the
Firm's value at run time, so a fresh deployment renders blanks rather than another firm's economics. Recording or
reading assumptions routes through the [`store::firm_capability`](../../store/src/firm_capability.rs) resolver: the
Owner and firm Admins may set them, other memberships are refused.

- Vocabulary: [`store::firm_assumptions`](../../store/src/firm_assumptions.rs)

The table is `firm_assumption`.
