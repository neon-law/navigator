---
title: "Discovery Request"
description: >-
  A Discovery Request is one served set of written discovery on a case, whose kind is interrogatories, requests for
  production, or requests for admission.
---

One served set of written discovery on a litigation case, with its numbered items and the responses and objections to
each. Its kind (`device`) is one of three:

- **Interrogatories** — written questions the responding party answers under oath.
- **Requests for production** — demands to produce documents or things for inspection.
- **Requests for admission** — statements the responding party must admit or deny.

A set is identified by its case, its kind, and its set number, and records who propounded it, who responds, when it was
served, and the response date the papers state. Each numbered item is a `discovery_item` row carrying the request text,
the response, and any objections. The response date is what the served papers say; the [Deadline](deadline.md) module
answers what is due.

A set may link the [Docket Entry](docket-entry.md) that records its service, but it is keyed to the case, not to the
entry. The `discovery_pairs` [Section](section.md) is the page block that shows a set's numbered request and response
pairs. It derives each item's status (needs a response, awaiting theirs, ready to sign, responded, closed) and its
sufficiency badge from what is recorded, attributes an objection to counsel and an answer to the responding party, and
adds the firm's "where it leaves us" note on the lawyer [Lens](lens.md) only.

- Commands: [`store::cases`](../../store/src/cases.rs) · Schema: [`discovery_request`,
  `discovery_item`](../../store/src/schema/navigator.surql)

The table is `discovery_request`.

Schema: [`discovery_request`](../../store/src/schema/navigator.surql#L2349).
