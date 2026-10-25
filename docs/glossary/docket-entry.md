---
title: "Docket Entry"
description: >-
  One typed, numbered entry on a litigation case's docket — the court's own record of what was filed or served.
---

One typed, numbered entry on a litigation case's docket — the court's own record of what was filed or served. The
generic spine every case record hangs off, mirroring how a court docket actually works: a numbered list of typed entries
rather than a table per instrument. A new niche instrument type is one value in the closed, code-extended `kind` set,
never a migration.

The entry number is **text, not an integer**. Real dockets use attachment sub-numbers such as `29-1`, and an entry list
has to reference them exactly.

A filed document is named for the entry's [DKT Number](dkt-number.md).

An entry with **no document attached is a meaningful state**, not an error: it renders as *source pending*.
Staged-versus-pending is derived from whether the document link exists, never from hand-maintained copy.

A **hearing** or **trial** records when the court set the appearance in `scheduled_on`. That field is required for those
two kinds and refused for every other. A continuance is a new docket entry of the same kind carrying the new date and a
`supersedes` link to the prior entry, so the earlier date stays on the record. The project calendar shows the entry no
later entry supersedes. Existing rows that predate `scheduled_on` are reported by a store read; they are never
backfilled. An appearance is a fact the docket records — it is not a Deadlines obligation.

**Docket belongs to litigation.** The case docket is the court record of filings. The cross-practice surface answering
*what is due* is the Deadlines module — litigators speak of *docketing* and corporate lawyers of a *compliance
calendar*, but each maps to one schema noun, the same way Matter maps to Project. This module records what exists; the
Deadlines module answers what is due.

**Read and write path.** `navigator project docket list|add|link-document|supersede` reads and writes a Project's
entries through `GET`/`POST /app/api/projects/{id}/docket` at Lawyer tier, scoped to a participant; `navigator project
docket ingest` records one entry per court notice through the same door. The matter page's calendar joins the docket
with the Project's open Deadlines in date order. The synced `case-timeline` Agent Skill is how a Project keeps its
timeline here rather than in a memo.

**On a Matter Page.** The [Case Timeline](case-timeline.md) (`docket_timeline`) [Section](section.md) renders the
entries with their documents and the open Deadlines in date order, beside `case_caption` and `fact_strip`. Each item's
side (ours, the other side's, or the court's) is derived from the filer, the client Entity and participants, and the
case's recorded posture, never typed; a stipulation counts for both sides and an entry with no filer stays unattributed.
A client's lens shows an entry only when its document is client-visible.

- Commands: [`store::cases`](../../store/src/cases.rs) · Schema:
  [`case_docket_entry`](../../store/src/schema/navigator.surql)
