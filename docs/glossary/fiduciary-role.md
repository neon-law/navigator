---
title: "Fiduciary Role"
description: >-
  A Fiduciary Role is one appointment an estate plan makes on a Project, naming who serves as trustee, executor, agent,
  or guardian and in what successor order.
---

One appointment an estate plan makes on a [Project](project.md): the role, the [Fiduciary](fiduciary.md) who serves in
it, and the successor order. The role is one of `trustee`, `successor_trustee`, `executor`, `alternate_executor`,
`agent_poa`, `health_care_agent`, or `guardian`. The fiduciary is one [Person](person.md) or one [Entity](entity.md),
such as a corporate trustee. Successor order starts at 1 for the first named, and a Project holds one row for each role
and order. A row may name the instrument Notation the appointment comes from.

A Fiduciary Role grants no lens on the matter. A fiduciary who reads a client's matter for them does so through a
delegation (`person_delegate`), and a fiduciary's role in an entity is an `entity_role` tie.

The rows belong to the `estate` [Module](module.md). They are imported from a Project repository's
`seeds/FiduciaryRole.yaml` by `navigator site import`. `GET /app/api/projects/{id}/fiduciary-roles` lists them for the
firm, and for the matter's client while the module is enabled.

- Store: [`store::fiduciary_roles`](../../store/src/fiduciary_roles.rs) · Schema:
  [`fiduciary_role`](../../store/src/schema/navigator.surql)

The table is `fiduciary_role`.

Schema: [`fiduciary_role`](../../store/src/schema/navigator.surql#L465).
