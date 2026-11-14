---
title: Receipt Document
description: A Receipt Document holds expense evidence in the Costs Matter Folder.
---

A Receipt Document is a [Document](document.md) with kind `receipt`. Its [Matter Folder](matter-folder.md) is `Costs` by
default. Uploads default to internal visibility.

The optional `receipt` block on its [Pointer](pointer.md) records `amount_cents`, `currency`, `payee`, `category`, and
one of `journal_entry_id` or `transaction_id`. The category is an [Expense Category](expense-category.md). To supply the
block, pass its YAML file to `navigator site document upload --kind receipt --receipt <yaml>`. Filing the Document
preserves the evidence without posting a [Project Cost](project-cost.md).
