---
title: Claim Packet
description: One PDF of a Legal Plan Claim's claim form, itemized invoice, and proof, filed on its Project.
---

A **Claim Packet** is what a [Legal Plan](legal-plan.md) that takes claims by email receives for one [Legal Plan
Claim](legal-plan-claim.md): the plan's claim form, an itemized invoice, and the proof the plan asks for, as one PDF.
`navigator project billing packet` renders it from the Project's plan link, the claim's verified terms and [Time
Entries](time-entry.md), and the documents filed on the Project. It is filed as an internal invoice document and linked
to the claim.

The plan's claim-filing rules are Firm data on the `legal_plan` row: where the claim is filed, the email subject, a
separate invoice address, the form's title, and the proof to include. The packet reads them; no plan is hard-coded.
[File a claim packet](../legal-plans.md#file-a-claim-packet) shows the commands.
