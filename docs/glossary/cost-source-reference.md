---
title: Cost Source Reference
description: The provider identity a Project Cost names for the transaction its money came from.
---

A **Cost Source Reference** is the provider identity a [Project Cost](project-cost.md) carries for the bank transaction
the money actually came from: `mercury::<transaction-id>` for a Mercury charge. It is required on every new Project
Cost, accepts only its canonical lowercase form, and is the dedup key a bulk import checks before drafting — a rerun of
the same window skips transactions already imported instead of double-charging them. The rule "never record an expense
without a Mercury source reference" is enforced here, in the store's validation, not in a reviewer's memory. It is
distinct from a [Source Reference](source-reference.md), which supports a [Journal Entry](journal-entry.md); a Cost
Source Reference names one bank transaction.

See also [Project Cost](project-cost.md).
