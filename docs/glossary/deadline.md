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

**Authority.** Each Deadline records why its date binds. The authority kinds are `statute`, `court_rule`, `case_law`,
`court_order`, `contract`, and `internal`. The authority citation identifies the governing source. Court dates enter the
[Deadline Proposal](deadline-proposal.md) queue with their filed evidence. The separate `source` field records the
producing workflow or the lawyer who entered the date.

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

**Due date and cutoff.** Use an RFC 3339 `--due-at` with an explicit offset for a timed obligation. For a civil due
date, use `--due-on YYYY-MM-DD` to inherit the matter’s time zone, or override it with `--due-timezone Area/City`. Add
`--cutoff HH:MM:SS` only when the source supplies a local cutoff. The API accepts the same facts in a `due_on` object
with `date` and optional `timezone` and `cutoff`. A civil date appears as an all-day event in Google Calendar and ICS.
The description includes any verified cutoff. Navigator uses the cutoff or the next local midnight for overdue checks.
The civil date stays the same in every viewer's time zone. Existing timestamp-only records keep their recorded instant.
A hearing requires a timed start. Save the notice’s duration with `--hearing-minutes` and its courtroom address with
`--hearing-location`. When the duration is unknown, the calendar reserves one hour.

**Scheduling.** Firm Settings sets the buffer in minutes, with a default of 15 and a minimum of 10. A timed hearing,
trial day, deposition, conference, consult, or call reserves Busy time on both sides of its scheduled duration. Filing
Deadlines have a submit-by target one buffer before their recorded cutoff. A civil date without a legal cutoff uses the
Firm's end of business in the matter's zone for its submit-by target. ICS preserves the recorded date and exports the
submit-by alarm or buffered appearance window.

**Effort and capacity.** Use `--effort 30m`, `--effort 4h`, `--effort 3d`, or `--effort 0` on creation or update. A day
represents eight hours of effort. Firm Settings supplies the court, discovery, internal, and tickler defaults when work
is first recorded. Navigator places the responsible attorney's work in free weekday hours beginning at 9 a.m. within the
internal lead window. The calendar plan covers 30 days and uses a seven-day lead when none is set. The brief and dated
agenda flag competing Deadlines at risk and show booked and available working time. Register a private [Attorney
Calendar](attorney-calendar.md) for Navigator to reserve work in Google Calendar.

**Whose date it is.** `calendar_role` is closed: `firm` (our obligation), `opponent` (the other side's deadline), or
`hearing` (an appearance). A hearing is not our filing obligation.

**Category.** A Deadline may be `court`, `hearing`, `discovery`, `internal`, `tickler`, `trial_day`, `deposition`,
`conference`, `consult`, or `call`. Creation defaults to `hearing` for the hearing role and `internal` for internal
authority. Supply the category when neither default applies. Google Calendar uses Tomato for court, Grape for hearings
and trial days, Banana for discovery and depositions, and Graphite for internal work and ticklers. ICS and Google
Calendar include the category label. Rows whose category is unsettled remain uncategorized.

`navigator project deadline list` marks any unset category with `category_review_required: true`. After review, use
`navigator project deadline update <id> --category <category>` to select the appropriate closed category.

**Status.** `status` is `open`, `complete`, or `cancelled`. Complete and cancel are transitions. Neither deletes the
row. Reopen restores a cancelled row to open and preserves its id, replay key, and stored fields. Reopening an open row
is a no-op; a complete row cannot be reopened. Replaying a cancelled row refuses without changing it and directs the
caller to reopen first. Replaying a complete row updates its fields while preserving complete status.

`navigator project deadline create` uses the supplied replay key or generates one for the request. If the response is
lost, Navigator reads the original Project and checks the key and requested fields. It returns the saved row when
confirmed. It makes at most two create attempts with the same key. An unconfirmed result includes the key to reuse.

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

Schema: [`project_deadlines`](../../store/postgres/schema.sql).
