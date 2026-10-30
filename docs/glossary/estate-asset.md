---
title: "Estate Asset"
description: >-
  An Estate Asset is one line of an estate plan's asset schedule on a Project, recording how the property passes and
  whether it is funded into the plan.
---

One line of an estate plan's asset schedule on a [Project](project.md). A line names the property by `category` and
`description` and records two facts about it:

- **How it passes** at death: `trust`, `beneficiary_designation`, `will`, `joint`, or `other`.
- **Its funding state**: `funded`, `in_progress`, `not_funded`, or `consent_needed`.

An optional note carries anything else. A Project holds one row for each category and description.

An Estate Asset is property, not bytes. Its table name distinguishes it from `asset`, the byte-pointer table for stored
documents and content (see [Asset](asset.md)); an Estate Asset never points at a stored file.

The rows belong to the `estate` [Module](module.md). They are imported from a Project repository's
`seeds/EstateAsset.yaml` by `navigator site import`. `GET /app/api/projects/{id}/estate-assets` lists them for the firm,
and for the matter's client while the module is enabled.

- Store: [`store::estate_assets`](../../store/src/estate_assets.rs) · Schema:
  [`estate_asset`](../../store/src/schema/navigator.surql)

The table is `estate_asset`.

Schema: [`estate_asset`](../../store/src/schema/navigator.surql#L482).
