---
title: "Matter Caption"
description: >-
  A Matter Caption is the matter block of a litigation Project's navigator.yaml as Navigator stores it: the court, case
  number, judicial officer, department, dates, parties, and counsel of record.
---

The facts every pleading in a litigation [Matter](matter.md) shares, declared once as the `matter:` block of the Project
[Repository](repository.md)'s `navigator.yaml`: the court, the case number, the judicial officer, the department, the
filing and trial dates, each caption [Party](party.md) with its role, and the [Counsel](counsel.md) of record. `notation
pdf` and `notation word` compose each pleading's caption from it, and `Y019` checks that it is complete.

`navigator project sync` stores the block with the Project, one per Project, so the matter page never reads git. The
`case_caption` and `party_roster` [Sections](section.md) prefer each fact it declares over the stored case row. A
manifest with no `matter:` key leaves the stored caption alone; `matter: {}` clears it. A caption party is caption text
and grants nothing.

- Shape: [`word::matter`](../../word/src/matter.rs) · Store:
  [`store::matter_captions`](../../store/src/matter_captions.rs) · Schema:
  [`matter_caption`](../../store/src/schema/navigator.surql) · Contract: [the gate](../gate.md#y-family--yaml-documents)

The table is `matter_caption`.

Schema: [`matter_caption`](../../store/src/schema/navigator.surql#L388).
