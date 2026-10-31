---
title: "Claims Agent"
description: >-
  The vendor administering claims in a bankruptcy case — Kroll, Epiq, Stretto — whose website hosts the case's key
  documents outside the court's own docket.
---

The vendor administering claims in a bankruptcy case — Kroll, Epiq, Stretto and their peers. In a Chapter 11 the
administrator of record for case documents is often the vendor, not the court: transfer notices, claim registers, and
amended filings are posted on the agent's website, and the court's own [NEF](notice-of-electronic-filing.md) for the
same event links there instead of hosting the PDF. The documents a matter needs are therefore frequently behind a
claims-agent portal and outside CM/ECF.

Navigator's `docket ingest` accepts a claims-agent notice alongside a CM/ECF notice. The sender is matched against the
known vendors' domains, including restructuring subdomains such as `restructuring.ra.kroll.com`. Every link whose label
names a [Docket Entry](docket-entry.md) number is fetched through the agent-fetcher seam and verified as a PDF by magic
bytes — the same check the CM/ECF free look applies — before it is filed into Docket Filings under its [DKT
Number](dkt-number.md) name with `claims_agent_notice` provenance.

When a link carries no entry number, or the fetch fails, the run degrades honestly: the document becomes a manual
handoff printed for a person, and its entry is recorded [source pending](docket-entry.md) rather than invented.
