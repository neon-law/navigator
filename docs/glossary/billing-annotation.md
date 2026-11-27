---
title: Billing Annotation
description: A Jujutsu change trailer that stages a timekeeper's work for review on a Project.
---

A **Billing Annotation** identifies one block of work by UUID, service date, and hours in tenths. The timekeeper stages
it with `navigator jj push` or `navigator jj stage-time`. Navigator retains the full change ID, original commit ID, and
authenticated timekeeper. A Project lawyer reviews it on the Finances tab. Approval records one [Time
Entry](time-entry.md) with the reviewed description. Denial retains the reviewer's reason.

The table is `billing_annotation`.
