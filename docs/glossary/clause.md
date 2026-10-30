---
title: "Clause"
description: >-
  A Clause is a block of legal prose reused by a Template with explicit inputs or added by a lawyer to one Notation.
---

A Clause supplies operative wording. The glossary defines the vocabulary used in that wording; it does not supply the
wording itself. A reusable Clause has a stable source, an explicit version, a permitted [Notation
Kind](notation-kind.md), and named inputs bound to questionnaire states. The bundled YAML library is its versioned
source of truth; the shared PostgreSQL `clause` table is a seeded read model. A [Template](template.md) includes it in
its body. Composition expands the prose before persistence, so an existing [Notation](../notation.md) keeps its saved
wording when the library changes.

A retired library version stays readable for provenance and cannot be newly referenced. Its `retired_at` date appears in
the read model and catalog; `N132` refuses new references. Retirement leaves existing expanded snapshots unchanged.

A lawyer may also add a matter-specific Clause to one Notation. Those paragraphs live in the [`notation_clause`
table](../../store/src/notation_clauses.rs) and are inserted at `{{custom_clauses}}`; they are separate from the shared
library. Both uses name legal prose, with different scopes.

See [Clause authoring](../clauses.md) for references, explicit jurisdiction inputs, and the source inventory.

The table is `notation_clause`.

Schema: [`notation_clause`](../../store/src/schema/navigator.surql#L1902).
