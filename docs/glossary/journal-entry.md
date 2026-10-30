---
title: "Journal Entry"
description: "A Journal Entry is one immutable, balanced economic event in a Firm's authoritative journal."
---

A Journal Entry belongs to one [Firm](firm.md). Its postings balance separately in each exact asset unit. A Project
allocation identifies the matter benefiting from a posting; it does not change the Firm that owns the journal.

Amounts are signed integer strings with explicit precision. Fiat currencies, network-qualified crypto tokens, and
identified noncash assets remain separate units. An award records an entitlement; a receipt records custody. An unknown
valuation remains unknown until dated evidence establishes a reporting value.

Source references identify provider objects within their tenant, account, and network scope. They may include transfer
locators and invoice relationships. References provide evidence; they cannot silently overwrite journal amounts.

Corrections append balanced entries. A reversal links and exactly negates its original entry. Client-reimbursable costs
require Project allocations and verified signed engagement evidence. Repayments also link the original costs.

Reconciliation compares explicit linked sources and retains checks and exceptions. Pending transactions, unavailable
providers, missing receipts, and incomplete period coverage cannot establish reconciliation. A Project statement covers
its allocations; it is not the Firm-wide IOLTA reconciliation.

The store command is `store::firm_journal::post`; records live in `journal_entry`.

The table is `journal_entry`.

Schema: [`journal_entry`](../../store/src/schema/navigator.surql#L2643).
