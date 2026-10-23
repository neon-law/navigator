---
title: "Fiduciary"
description: >-
  A Fiduciary is a person who acts for another under a duty of loyalty and care, such as a trustee, an executor, a
  guardian, or an agent under a power of attorney.
---

A person who holds property or acts for another under a duty of loyalty and care: a trustee for a trust's
[Beneficiary](beneficiary.md), an executor or personal representative for an estate, a guardian for a ward, or an agent
under a power of attorney. In [Estate Administration](estate-administration.md) and [Trust
Administration](trust-administration.md) the fiduciary is usually the client the firm advises.

When a fiduciary reads a client's matter for them, the authority is a delegation (`person_delegate`) whose basis names
the instrument behind it — `trustee`, `executor`, `guardian`, or `power_of_attorney`. When a fiduciary holds a role in
an entity, such as trustee of a trust that owns a company, it is an `entity_role` tie. Who an estate plan appoints, and
in what successor order, is a [Fiduciary Role](fiduciary-role.md) on the Project.

- Delegation: [`store::delegations`](../../store/src/delegations.rs) · Schema: [`person_delegate`,
  `entity_role`](../../store/src/schema/navigator.surql)
