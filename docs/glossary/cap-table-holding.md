---
title: "Cap Table Holding"
description: >-
  A Cap Table Holding is one holder's shares of one class of a company's equity, recorded on a Project whose cap table
  is in play.
---

One holder's shares of one class of a company's equity, recorded on a [Project](project.md) whose [Cap
Table](cap-table.md) is in play. The holder is one [Person](person.md) or one [Entity](entity.md). The row carries the
share class, a whole number of shares, and an optional vesting note. A Project holds one row for each holder and class.
Shares are never stored as a percentage; a reader derives ownership from the rows.

The rows belong to the `cap_table` [Module](module.md). They are imported from a Project repository's
`seeds/CapTableHolding.yaml` by `navigator site import`. `GET /app/api/projects/{id}/cap-table-holdings` lists them for
the firm, and for the matter's client while the module is enabled.

- Store: [`store::cap_table_holdings`](../../store/src/cap_table_holdings.rs) · Schema:
  [`cap_table_holding`](../../store/src/schema/navigator.surql)

The table is `cap_table_holding`.

Schema: [`cap_table_holding`](../../store/src/schema/navigator.surql#L499).
