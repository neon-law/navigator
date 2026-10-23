---
title: "Party"
description: >-
  A Party is a person or entity with a side in a matter, recorded as a caption party, a project reference, or a
  participant.
---

A [Person](person.md) or [Entity](entity.md) with a side in a [Matter](matter.md). Navigator records a party in one of
three places, by what the record is for:

- **On a pleading's caption** — the `parties` list of the manifest's `matter:` block, each with its name and its role in
  the case. It is caption text, checked by `Y019`, and grants nothing.
- **As a counterparty** — a [Project Reference](project-reference.md): a verified Person or Entity, such as the adverse
  party or its registered agent, with no standing on the matter. The conflict check can collide a later matter with it.
- **As a participant** — a [Person–Project Role](personproject-role.md), for the client side and the firm side. Only
  this one grants a lens on the matter.

Keeping the three apart is what stops a counterparty named on a caption from ever reading the matter.

- Caption: [`word::matter`](../../word/src/matter.rs) · Schema: [`project_reference`,
  `person_project_role`](../../store/src/schema/navigator.surql)
