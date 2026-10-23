---
title: "Beneficiary"
description: >-
  A Beneficiary is a person or entity entitled to benefit from a trust, an estate, an account, or a policy that a
  fiduciary administers.
---

A [Person](person.md) or [Entity](entity.md) entitled to benefit from a trust, an estate, an account, or a policy. A
[Fiduciary](fiduciary.md) administers the property for them; the beneficiary does not control it.

A beneficiary of an entity, such as a trust, is an `entity_role` tie with the role `beneficiary`, which the conflict
check treats like any other structural tie. A beneficiary named in an estate plan is an answer to a notation's
questionnaire (for example, a residuary beneficiary), not a participant: being named in a plan grants no lens on the
matter.

- Schema: [`entity_role`](../../store/src/schema/navigator.surql) · Related: [Probate](probate.md), [Trust
  Administration](trust-administration.md)
