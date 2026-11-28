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

To post a confirmed, unbilled cost from a filed receipt with a canonical Mercury transaction ID, use the Receipt
Document UUID and the date the charge occurred.

```bash
navigator project cost create --project <code> --receipt <document-uuid> --occurred-on YYYY-MM-DD --confirm
```

The cost appears under **Finances > Expenses**. If the Project already has a cost for that transaction, the command
reports the existing cost ID and skips creation.
