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

- Section: [`webapp::sections::timeline`](../../webapp/src/sections/timeline.rs) · Contract:
  [Docket Entry](docket-entry.md)
