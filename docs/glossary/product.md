---
title: "Product"
description: >-
  A preserved price and accounting entry from the historical billing catalog.
---

A Product records the catalog code, display name, billing kind, cadence, list price in minor units, currency, and
account code. Its optional Xero item and template codes preserve the associations recorded by that catalog.

The PostgreSQL table is `product`. The first [Mirror Backfill](mirror-backfill.md) copies it by source UUID. Live client
purchases use [Offerings](offering.md).

Schema: [`product`](../../store/postgres/schema.sql).
