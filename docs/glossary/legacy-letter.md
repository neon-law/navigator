---
title: "Legacy Letter"
description: "A Legacy Letter is an agreement Draft with an onboarding shared-template lineage."
---

An engagement Draft that declares `kind: agreement` and a `derived_from` field naming an `onboarding__*` shared
template. The Project's engagement Matter Page selects its declared Draft before any lifecycle-kind fallback. `navigator
project onboard` requires the letter to be migrated rather than opening an agreement as an engagement or writing another
stub beside it.

`navigator project migrate-letter` retypes the Draft as `onboarding`, moves it to the canonical onboarding path and
updates its Matter Page references. The `derived_from` field stays documentation-only: the rule engine never resolves
it, and the [Migration Verb](migration-verb.md) preserves it with the body. An agreement with an unrelated lineage is
not a Legacy Letter and cannot be migrated into an onboarding.
