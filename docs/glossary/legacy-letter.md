---
title: "Legacy Letter"
description: "A Legacy Letter is a Project's engagement letter declaring kind: agreement and a derived_from lineage."
---

A Project's engagement letter drafted against the pre-lifecycle layout: the draft declares `kind: agreement` in its
frontmatter and records a `derived_from` field naming the shared template it was authored from. Navigator's lifecycle
commands open letters by their lifecycle `kind` (`onboarding`, `offboarding`) at the canonical draft path, so a Legacy
Letter that should open a Project never does on its own — `navigator project migrate-letter` moves it onto the canonical
onboarding path instead. The `derived_from` field stays documentation-only: the rule engine never resolves it, and the
[Migration Verb](migration-verb.md) preserves it when retyping the draft.
