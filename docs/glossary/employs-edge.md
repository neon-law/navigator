---
title: "Employs Edge"
description: >-
  An Employs Edge records that an entity employs a person.
---

The Relationship Edge kind `employs`: A employs B. It records employment, including a counterparty's officer and a
client's contact person. Governance and ownership are not employment: an officer's seat or an owner's stake is a
[Person–Entity Role](personentity-role.md) (`officer`, `owner`), recorded beside the `employs` edge where both hold.

- Kind: `employs`, one value of the closed [Relationship Edge Kind](relationship-edge-kind.md) vocabulary.
- Direction: A (the employer) to B (the employee).
- Endpoints: A is an [Entity](entity.md); B is a [Person](person.md).
- Example: A company employs an analyst.
- Retired aliases: `client_contact` (the default `entity_role` bulk import writes) and `counterparty_officer` (a
  [Project Reference](project-reference.md) reason) map to `employs`.
- Scope: carries the three scope anchors every [Relationship Edge](relationship-edge.md) carries — the Project it was
  learned in (optional), the attorney who recorded it, and the Firm — and appears in the Project, attorney, and Firm
  conflict graphs. This is the target design, not yet implemented.
