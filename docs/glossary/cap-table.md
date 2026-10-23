---
title: "Cap Table"
description: >-
  A Cap Table is the record of who owns what share of a company, which the firm keeps outside Navigator and summarizes
  on a Project as Cap Table Holdings.
---

The record of who owns what share of a company: its classes of equity, the holders of each, and the instruments that
convert into them. The firm keeps the full cap table, with its instruments and history, outside Navigator.

A [Project](project.md) can enable the `cap_table` [Module](module.md), which marks the matter as one where the entity's
ownership is in play. The module owns the Project's [Cap Table Holding](cap-table-holding.md) rows: each holder's shares
of each class, imported from the Project repository's seeds. Navigator keeps no ledger of issuances or transfers.

- Module: [`store::project_modules`](../../store/src/project_modules.rs) · Holdings:
  [`store::cap_table_holdings`](../../store/src/cap_table_holdings.rs) · Schema: [`project_module`,
  `cap_table_holding`](../../store/src/schema/navigator.surql)
