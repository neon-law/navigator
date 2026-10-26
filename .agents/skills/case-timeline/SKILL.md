---
name: case-timeline
description: >
  Keep a litigation matter's case timeline as Docket Entries in Navigator, one per entry on the court's docket, each
  linked to the filed document, with hearings and trials carried as dated entries and continuances superseding the dates
  they move. Trigger whenever a filing, order, notice, hearing, or continuance arrives in a matter, when a case
  timeline, docket log, or "current position" is asked for, or before drafting anything that depends on what has been
  filed. Kept identical, byte for byte, in every Project repository's `.agents/skills/`; this canonical copy lives in
  Navigator's own `.agents/skills/`.
---

# Case timeline

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`, then read
the terms themselves:

```bash
navigator glossary show "Docket Entry"
navigator glossary show "Deadline"
```

A litigation case record is its docket: a numbered list of typed entries, each naming what was filed or served, by whom,
on what date, and the document on file for it. Navigator stores that list as Docket Entries, and the matter page renders
them joined with the matter's Deadlines in date order. The timeline is never a hand-edited memo: a memo goes stale the
week the other side files, and a table the store already models is not kept twice.

## 1. Read what is already recorded

```bash
navigator project docket list
```

The command reads the Project code from `navigator.yaml` and prints every case on the matter with every entry on its
docket, in the order the court's docket is read. Match a new filing against existing rows by entry number before
writing. An entry number already on the case is the same entry; recording it again reads the existing row back.

## 2. File the document first

A Docket Entry names the document on file for it. File that document before recording the entry, so the entry is never
*source pending* longer than it has to be:

- A court's CM/ECF notice: `navigator project docket ingest <notice.eml>` files the free-look PDF under
  `documents/Docket Filings/DKT NNNN - <title>.pdf`, its Pointer under `blobs/`, and records a linked Docket Entry for
  the main document and each numbered attachment.
- Our own filing, or a document that arrived another way: `navigator project sync` from the checkout, or `navigator site
  document upload --kind filing`, then record the entry below and pass the document's slug (its path under `documents/`)
  as `--slug`.

An entry with no document is a meaningful state, not an error. Record it, and link the document when it arrives.

## 3. Record the entry

```bash
navigator project docket add \
  --entry-number 29 \
  --kind motion \
  --title "MOTION to Dismiss for Failure to State a Claim" \
  --party "Defendant Example Co." \
  --filed-on 2026-03-05 \
  --slug motion-to-dismiss.pdf
```

- **The entry number is the court's**, as text: `29`, or `29-1` for an attachment. Never renumber.
- **The kind is closed.** Pick from `pleading`, `motion`, `opposition`, `reply`, `order`, `notice`, `stipulation`,
  `discovery_request`, `discovery_response`, `subpoena`, `expert_disclosure`, `hearing`, `trial`, `appeal`,
  `settlement`, and `other`. `other` is for an entry that is genuinely none of the rest, not a shortcut.
- **The case comes from `navigator.yaml`.** The entry joins the case named under `dockets:` (or `matter.case_number`).
  With several, pass `--docket <case number>`. A matter with no case row yet gets one opened from that number on the
  first write, captioned from `matter:`.
- **`--dry-run` prints the request and writes nothing.** Use it to check the entry before it is recorded.
- The command applies the Docket Entry's DKT name to its linked Document and prints the stored row. Read it back and
  confirm the number, kind, title, and date before going further.

Link a document to an entry recorded before the document was on file:

```bash
navigator project docket link-document <entry id> --slug motion-to-dismiss.pdf
```

## 4. A hearing is a Docket Entry

The docket owns an appearance date. Record a hearing or trial as an entry of that kind with the date the court set:

```bash
navigator project docket add \
  --entry-number 31 \
  --kind hearing \
  --title "Hearing on motion to dismiss" \
  --scheduled-on 2026-05-14T09:00:00-07:00
```

`--scheduled-on` is RFC 3339 with an explicit offset, in the court's time zone. It is required for a hearing or trial
and refused for every other kind. A continuance is a new entry, never an edit: the earlier date stays on the record and
the matter page follows the chain to the entry no later entry supersedes.

```bash
navigator project docket supersede <hearing entry id> \
  --entry-number 34 \
  --scheduled-on 2026-06-11T09:00:00-07:00 \
  --slug docket/9-26-cv-00042/di-34-order-continuing-hearing.pdf
```

Do not record a continuance until the order or the signed stipulation exists; file it and pass it as `--slug`.

A date the entry *sets for a party* — a response due, a brief due, a deadline the order imposes — is not a Docket Entry.
Hand it to the `deadline` skill, which records it once through `navigator project deadline` with its authority. The
`deadline` skill's `--calendar-role hearing` remains for a reminder about an appearance, but the docket is where the
appearance's date lives; a Deadline row never stands in for the entry.

## 5. Check the matter page

Open `/app/projects/<code>` and confirm the calendar lists the entry with the right date and status (`Filed`, `Source
pending`, `Hearing`, `Trial`, or `Superseded`) beside the matter's open deadlines, in date order. A client sees a Docket
Entry only when its Document is client-visible, and a deadline only when it has a client lead time.

## 6. Calls and other off-system contact

A phone call with opposing counsel, a court clerk, or the client is a Communication, not a document. Log it from the
note you took, and it joins the matter's conversation beside the email and text it belongs with:

```bash
navigator project communication create \
  --channel call \
  --direction outbound \
  --counterparty counsel@example.com \
  --role opposing_counsel \
  --at 2026-10-09T15:00:00-07:00 \
  --duration 20 \
  --meet-and-confer "Responses to Set One" \
  --agreement "Responses extended to 2026-11-02" \
  --body-file notes/2026-10-09-call.md
```

- **Internal by default.** A logged call is an internal note the client never sees. A call with `--role
  opposing_counsel` is always internal; `--client-visible` is refused for it.
- **Agreements are tracked until confirmed.** Each `--agreement` stays `unconfirmed` until the confirming email is
  logged with `--confirms <call id>`. Send that email the same day, then log it: `navigator project communication create
  --channel email_outbound --direction outbound --counterparty … --at … --body-file … --confirms <call id>`.
- **A date agreed on a call is a Deadline.** Record it through the `deadline` skill once the confirming email exists;
  the call note is not the record of the date.
- **Communications stay server-only.** Nothing is written under `documents/` or `blobs/`, `project sync` writes no
  pointer for one, and a call is never uploaded as a `memo` or `transcript` document. Read the log back with `navigator
  project communication list --channel call`.

## 7. Current position

When asked where the case stands, read the docket (step 1) and answer from it: the last entry, the next appearance, and
the open deadlines. Keep client identities and matter facts out of any surface the private channel does not already
hold. Do not write a memo that restates the docket; link to the matter page instead.

## 8. Report what succeeded

End with what actually happened on each surface: each Docket Entry (id and read-back), the documents linked, and the
matter page. If a document could not be filed or an entry was refused, say so plainly and name the refusal.
