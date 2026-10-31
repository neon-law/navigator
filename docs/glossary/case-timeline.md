---
title: "Case Timeline"
description: "A Case Timeline presents a Project's Docket Entries and open Deadlines in date order."
---

The litigation [Matter Page](matter-page.md) section that presents [Docket Entries](docket-entry.md) with their linked
[Documents](document.md) and the Project's open [Deadlines](deadline.md) in date order. Its Section key is
`docket_timeline`. Its dates come from stored records. The side filter selects both sides, our side, the other side, or
the court.

The timeline reads the Project's docket and Deadlines. The `navigator project docket` commands and the canonical
`case-timeline` Agent Skill maintain its Docket Entries; [Docket Filings](docket-filings.md) holds the local document
bytes.

Every Docket Entry also renders a **Case Timeline record** in the checkout: the file `documents/Case Timeline/DKT NNNN -
TITLE.yaml`, named the way [Docket Filings](docket-filings.md) names its filings. The record is a rendering the store
owns — every docket write (`add`, `ingest`, `link-document`, `supersede`) regenerates it from the row the store read
back, and `navigator project sync` re-renders the whole folder from the live docket, removing a record whose entry is
gone only with `--yes-delete`. A record is never hand-edited: the Project gate refuses one that disagrees with its
Docket Entry, and the repair is `navigator project sync`, not an edit.

- Section: [`webapp::sections::timeline`](../../webapp/src/sections/timeline.rs) · Contract:
  [Docket Entry](docket-entry.md)
