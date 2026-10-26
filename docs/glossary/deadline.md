---
title: "Deadline"
description: "A Deadline is a forward-dated obligation on a Project, such as a pleading or filing due date."
---

A forward-dated obligation on a [Project](project.md) — a pleading due, a filing due, a statutory window closing. This
is the firm's docket: a missed deadline is a malpractice event, not a backlog item. Deadlines are the one
forward-looking record in the schema. [Notation Event](notation-event.md) is an immutable journal and is past tense by
construction, and [Filing](filing.md) names a workflow step that runs, not a date it is due by.

The table is `project_deadlines`. [Matter](matter.md) is client-English for the same row, and lawyer-facing copy may say
"matter deadline", but the schema speaks `project` without exception — every other table already does.

**Title.** `title` is an optional short name for what is due — an answer, a reply brief — of at most 200 characters. A
row recorded without one reads back with no title. The matter page, the `deadline_board`, and the Project's ICS calendar
show the title when a row has one. Clearing it returns the row to untitled.

**Description.** `description` is an optional explanation of what is due. Empty input clears it. It follows the
Deadline's visibility on the matter page and board.

**Visibility.** `--visibility internal|client` on the CLI expresses the existing client lead rule. Creation defaults to
internal. Choosing `internal` clears the client lead. Choosing `client` requires an explicit client lead and its
internal lead. Updates preserve visibility unless a new choice is supplied. No second visibility field is stored.

**Authority.** Every deadline records *why the date binds*: a closed `authority_kind` vocabulary — `statute`,
`court_rule`, `court_order`, `contract`, or `internal` — beside a free-text `authority` citation such as `15 U.S.C. §
1681i(a)(1)` or a court-order reference. "Statutory" stays a queryable distinction rather than collapsing into a general
bucket, and a deadline nobody can justify cannot be written: `statute` and `court_rule` both require a citation.
Distinct from `source`, which records the *producing workflow* — or `lawyer` for a hand-entered date — not the
authority.

**Stored, never derived.** The due date is written down, not recomputed at read time. A rule can change — a statute is
amended, a court rule is revised — and recomputing would silently move a date the firm already docketed and relied on.
The stored date is the one malpractice exposure attaches to; `authority` records the rule that produced it, so the
derivation stays auditable. Computing court days (per jurisdiction, with holiday calendars and service-method
extensions) is deliberately out of scope — a deadline must never *pretend* to have counted court days.

**Two lead times.** A deadline carries separate internal and client lead times, because the firm is warned before the
client is: the internal lead is never shorter than the client lead. Either may be unset, and an unset client lead means
the client is never warned about that date.

**Idempotency is explicit.** A workflow-written deadline carries a `replay_key` so a replayed [`ctx.run`](ctxrun.md)
step updates its row instead of duplicating it. A hand-entered deadline leaves the key unset, so two genuinely different
pleadings sharing a kind and a trigger date stay two rows — an idempotency key, not a natural key that silently merges
malpractice-relevant records.

**Stored due instant.** `due_at` is the stored due date as an RFC 3339 timestamp with an explicit offset (`Z` or
`±hh:mm`). A date with no offset is refused. The field is never a court-day count.

**Whose date it is.** `calendar_role` is closed: `firm` (our obligation), `opponent` (the other side's deadline), or
`hearing` (an appearance). A hearing is not our filing obligation.

**Status.** `status` is `open`, `complete`, or `cancelled`. Complete and cancel are transitions. Neither deletes the
row. Reopen restores a cancelled row to open and preserves its id, replay key, and stored fields. Reopening an open row
is a no-op; a complete row cannot be reopened. Replaying a cancelled row refuses without changing it and directs the
caller to reopen first. Replaying a complete row updates its fields while preserving complete status.

**Calendar revision.** `calendar_sequence` is an internal, monotonic RFC 5545 integer. A new row starts at zero; updates
and status changes increment it atomically. Historical rows keep their timestamp-based exported version until the next
write increments from that version. Values above 2147483647 are refused.

**Who owns it.** `owner_person_id` is the optional Person responsible for the date.

**Where it was read.** `source_document_id` is the optional Document ([Asset](asset.md)) that records the date. It is
not `source`: `source` is still the producing workflow, or `lawyer` for a hand-entered date.

**Lead times, in days.** `internal_lead_days` and `client_lead_days` are the internal and client lead times as whole
days. The internal lead is never shorter than the client lead. A client lead requires an internal lead. Either may be
unset when the client lead is unset.

**Where it shows.** The matter page's *Deadlines* panel lists every open row, soonest first, with the weekday and the
stored local offset. It marks overdue rows and dates within the viewer's lead time (internal or client), shows the
calendar role and authority kind as chips, and lets the reader expand the authority citation. Complete and cancelled
rows remain in a collapsed *Past* section. A client's view lists only rows that carry a client lead time, including in
*Past*; an internal date never reaches it. The synced `deadline` Agent Skill records a date through `navigator project
deadline` and reminds the Project's private Slack channel.

**Hard or soft.** A Matter Page's `deadline_board` marks each row *hard* when a statute, court rule, court order, or
contract binds the date, and *soft* for an internal target. The mark is read from `authority_kind`, never stored.
`key_dates_strip` lists the next dates still ahead, with a client's lens measuring *due soon* against the client lead.

- Panel: [`webapp::project_deadlines`](../../webapp/src/project_deadlines.rs) · Store:
  [`store::project_deadlines`](../../store/src/project_deadlines.rs)
