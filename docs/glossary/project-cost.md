---
title: Project Cost
description: A third-party charge recorded for a Project with a linked receipt Document.
---

A **Project Cost** is a third-party charge recorded against a [Project](project.md), backed by a live receipt
[Document](asset.md). Its amount may be estimated or confirmed; its billing state records whether it remains unbilled,
was linked to a later invoice, or was resolved another way. Every cost names the bank transaction its money came from
through its [Cost Source Reference](cost-source-reference.md). The record keeps its own audit history and never changes
an invoice in Xero.

Confirmed, unbilled costs pass through to a matter invoice at their recorded amount, with quantity one. They remain
separate from the legal-services fee, including when an agreed flat fee covers the work. The invoice review binds the
cost's amount, description, payee, and date; changing those inputs requires a fresh review before generation.

Project Finances shows the [Expense Category](expense-category.md) from the receipt and links to the receipt while it is
available. Costs with legacy documents that carry no category appear under Other expense. Project Finances totals
confirmed, unbilled costs by category and currency. Firm Finances shows the same costs across the viewer's participating
matters. A retained cost whose receipt is unavailable is marked Receipt missing.

The table is `project_costs`.
