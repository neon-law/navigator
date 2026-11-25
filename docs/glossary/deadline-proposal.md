---
title: "Deadline Proposal"
description: "A Deadline Proposal is a cited date for a lawyer to review before recording a Deadline."
---

A candidate court date in a [Project](project.md). An express date cites a signed order. A calculated date cites the
triggering record and a filed [Authority](authority.md). Each citation identifies an original file version, its digest,
a page, and the highlighted passage.

The table is `deadline_proposal`. Each row preserves the submitted revision and its source receipts. The lawyer's
decision and the publication status belong to that revision.

A lawyer or supervised clerk can prepare a proposal. The reviewing lawyer confirms the displayed revision or returns it
for correction. DBOS publishes the approved revision as a [Deadline](deadline.md). Until approval, a proposal stays in
the review queue. A later amendment keeps the existing effective date until the lawyer confirms the change.

[Queue and review court dates](../deadline-proposals.md) describes the CLI, evidence and keyboard controls.
