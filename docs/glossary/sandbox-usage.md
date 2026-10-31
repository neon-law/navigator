---
title: "Sandbox Usage"
description: "One subscriber's metered model-usage credit for one billing month in the Legal Sandbox."
---

The metered-credit ledger behind the [Legal Sandbox](legal-sandbox.md): one row per [Neon Law
Nautilus](neon-law-nautilus.md) subscriber per billing month. A row holds the month's granted model-usage credit (500
cents — the subscription's $5.00), the running debit across the month's completions, and the overage flag. Money is
integer cents, never a float, matching the billing contract.

The crossing completion completes: the debit that pushes the balance negative succeeds and flags `overage`, and the next
completion is refused until the à la carte charge is settled — pricing provided by Lex Tecnica. The row's record id is
the compound `person:period` key, so two parallel tabs debit the same row and cannot double-spend. A fresh month's grant
is a new row; a closed month's overage row is never rewritten, so it survives as invoice-line-ready data.

The table is `sandbox_usage`.

The month's credit is delivered by the **sandbox credit reset** — the `SandboxCreditReset` Restate workflow in
`billing-workflows`, fired monthly by the `sandbox-credit-reset-trigger` `CronJob`. It grants every active subscriber
the fresh period's credit and converts the closed month's overage rows into à la carte invoice lines through the
[`billing`](../../billing/README.md) contract, keyed on a deterministic per-subscriber-month invoice id so a retry never
bills twice.

Schema: [`sandbox_usage`](../../store/src/schema/navigator.surql#L2686).
