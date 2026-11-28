---
title: "E-Filing Transaction"
description: >-
  One submission an e-filing service provider (EFSP) accepted for delivery to a court, recorded on a Project with the
  status the provider last reported.
---

One submission an e-filing service provider (EFSP) accepted for delivery to a court, recorded on a [Project](project.md)
with the status the provider last reported. A state court that takes filings only through approved EFSPs never answers
the filer directly. A notice names a **transaction id** or, when that field is absent, an **envelope number**. The
recorded identifier is the row's key on its Project, so another report for the same identifier updates the row.

The table is `efiling_transactions`. It is distinct from the [E-Filing](e-filing.md) workflow step, which submits; this
row records what the provider says happened afterwards.

**Status.** `status` is closed: `submitted` (sent, no answer yet), `accepted`, `rejected`, or `unknown`. `unknown` is
what a notification-only email produces — one that names the transaction but not its status — and its note says to log
in to the EFSP to read it. A report without a decision (`submitted`, `unknown`) never replaces a recorded `accepted` or
`rejected`.

**Filed is not accepted.** `filed_on` is the date the court deems the document filed; `accepted_on` is the date the
clerk accepted it. A court can deem a document filed on the day it was received even when acceptance comes later, so the
two are stored separately. Both, and the [Conformed Copy](conformed-copy.md), are refused unless the status is
`accepted`: a pending filing is never recorded as filed.

**Which version.** `submitted_document_id` names the submitted revision ([Asset](asset.md)), and `conformed_document_id`
names the court-stamped return. Both must be on the same Project.

**Judge-signed returns.** A proposed order comes back from the EFSP only as a *received* copy. `needs_entered_order`
marks the row until the entered order is obtained.

**Filing fees.** An accepted envelope can link one internal [Receipt Document](receipt-document.md) for the observed
fee. Until the Firm verifies a settled card charge, the amount stays pending on the envelope. Settlement records the
reviewer and the exact bank source privately. It creates or links one confirmed [Project Cost](project-cost.md) and
preserves the receipt link in Finances.

- Commands: `navigator project efiling record` and `navigator project efiling pair` · Store:
  [`store::efiling_transactions`](../../store/src/efiling_transactions.rs)

Schema: [`efiling_transactions`](../../store/postgres/schema.sql).
