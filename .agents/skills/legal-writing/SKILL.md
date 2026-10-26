---
name: legal-writing
description: >
  Fleet-wide discipline for legal writing that becomes work product (memos, letters, briefs, and responses): the answer
  first, plain language, every sentence proved against a saved source, and argument written for its reader. Trigger
  whenever drafting or revising a memo, client letter, brief, or written response the firm will stand behind. Kept
  identical, byte for byte, in every Project repository's `.agents/skills/`; this canonical copy lives in Navigator's
  own `.agents/skills/`. Not the [[legal-council]] copy-review bench (that shapes notation-bound template language
  before it hardens) and not [[litigation-filing-review]] (that is the attorney-of-record filing gate); this is the
  standard both of them assume the underlying prose already meets.
---

# Legal writing

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

Follow [[human-readable]]: answer first, one idea per sentence, jargon defined inline, and short lists for prose that
does not fit a compact table. Write for the client or lawyer who must act on the answer.

## Memo notations

A memo is a notation. Author its reusable source at `templates/<code>.md` as `kind: memo` in the Project repository. Use
the Project's chosen `drafts/` directory when its blueprints live there. Keep client data and matter facts out of Git:
use placeholders for facts and keep their answers, cited originals, and produced work product in Navigator.

Enclose these minimal frontmatter keys in YAML delimiters, using the memo's title and governing jurisdiction:

```yaml
kind: memo
title: Contract review memo
respondent_type: person
jurisdiction: NV
confidential: true
output: letter
```

The filename supplies the code. A body-only memo needs neither `questionnaire:` nor `workflow:`.

Run `navigator project gate`, review and commit the source, then push a preview with `navigator notation preview
templates/<code>.md`. Preview stores a draft for review; it does not run a notation or produce a filed document. Do not
substitute a document upload for authoring the notation's source.

Keep lines within 120 columns. Use short lists for prose cells that make a table too wide. Preserve exact statute
numbers and citations when reflowing; verify the gate's edits before committing. Save and check each cited original
through the source-preservation procedure below.

## Page one carries the decision

Open a memo, letter, or brief with the answer and recommended action. Name the decisive fact, any uncertainty that
changes the action, the deadline, and the ask. A reader who stops after the first paragraph still knows what to do.

Recommend one course. Refuse "it depends" unless the next clause says what it depends on and when the matter will know:
a fact still to be confirmed, a ruling still pending, a deadline not yet reached. "It depends, and we will know once
opposing counsel answers the interrogatory due the 14th" belongs on page one. "It depends" alone declines to write.

## Every sentence is proved

Every sentence is proved and every source saved. Every fact in the draft carries one of three labels, visible to the
supervising lawyer before the draft becomes work product:

- **Documented:** pinned to a source with a pinpoint cite (page, paragraph, or exhibit line).
- **Client-asserted:** attributed to the client's own account, not independently verified.
- **Unknown:** named as a gap, not silently assumed.

Open, quote verbatim, and check the treatment of every authority before relying on it. Never cite from a snippet, a
headnote, or memory. State odds as bands tied to named fact scenarios ("likely if the signature is authenticated,
unlikely if it is not"), never as a bare point estimate. The band tells the next reader which fact would change it.

**Nothing goes in a filing the lawyer does not believe true.** Candor comes before advocacy.

### Source preservation

Save every cited source in its original bytes, not just a link, so the supervising lawyer can verify it. A citation with
no saved original is flagged as unverifiable and goes no further until it is saved.

- **Law** (a case, statute, regulation, or rule) is saved as an Authority: `navigator site authorities create
  --class <class> --citation <citation> --title <title> --file <archived-copy>`. Set `--checked-on` to the date the
  source was actually opened and checked, not the date the memo was drafted. An Authority is global reference data
  shared by every matter that cites it, never a bare git-committed file.
- **Matter-specific material** (a filed document, a produced page, an opposing brief, correspondence) is saved as a
  matter document, scoped to its matter, never as a global Authority.
- No field yet records the date and source a matter document was captured from. Record both in `--description`. Do
  not invent a workaround field or convention.

## Filing a document into a matter

Filing is the most common operation in a Project repository and the one an agent most often does unattended. A wrongly
filed client document is worse than an error, so do not guess.

- **Repository coordinate.** A Project has one repository, stored as a whole URL on the Project
  (`project.repository_url`), never composed from a host, an organization, and the Project code. Read it off the
  Project's show page in the portal, which renders the stored URL verbatim (or nothing). No CLI command prints it:
  `navigator project doctor --project <code>` is an offline diagnosis and does not report it. If no URL is recorded, do
  not guess an organization.
- **Direct upload versus staged sync.** `navigator site document upload --project <code> --file <path> --kind <kind>`
  files one document immediately through the API; use it for a single filing during a session. `navigator project sync`
  is the batch path: it uploads files staged under a checkout's `documents/` folder, which the root `.gitignore` keeps
  out of Git, and writes each document's pointer under `blobs/`.
- **Direct upload records a Pointer in its Project checkout.** When `navigator site document upload` runs from
  the matching Project repository, it writes only `blobs/<slug>.yaml` and also prints the Pointer. The legal document
  bytes stay in Navigator storage. Review and commit the pointer; run `navigator project gate --check` to reconcile
  every live row this login can see, or `navigator project sync` to catch up the whole matter.
- **Outside the matching checkout**, direct upload prints the Pointer without writing a file. Run it from the
  Project repository when you want the Pointer staged there.
- **Filename.** No convention is enforced on `--file`. Use a stable name a lawyer would recognize on sight (the caption
  or subject, compressed), since `--slug` defaults to it.
- **Kind.** One of `letter`, `filing`, `will`, `trust`, `directive`, `agreement`, `pleading`, `onboarding`,
  `offboarding`, `memo`, `transcript`, `inbound_contract`, `certificate_of_naturalization`, `exhibit`, `order`,
  `closed_repository`, `invoice`, `unclassified` (`navigator site document upload --help` prints the same list).
  `filing` is a document filed with a government body; `pleading` is a court paper (a complaint, motion, or brief) filed
  with a court. A scanned motion is `filing` only before it is filed with the court. A court's own order is `order`.
- **Visibility.** `--visibility internal` (default) or `--visibility client`, the client-visible lane.
- **Metadata.** `document upload` has no `--metadata` flag. The committed pointer carries only `kind`, `visibility`,
  and the current revision's `version`, `asset_id`, `created_at`, `sha256`, and `size_bytes`. `--description` lands on
  the server-side asset row, not the pointer; record capture provenance there (see *Source preservation*).
- **Source extension.** `--slug` must end in the uploaded file's own extension, case-insensitively (`--slug
  motion.pdf` for `motion.pdf`, never a bare `motion`). A slug that drops or mismatches the extension is refused before
  any network call, naming the fix:

  ```text
  document slug `motion` must carry the source extension from `motion.pdf`; use `motion.pdf` so it can be
  represented as a Pointer
  ```

- **Pointer path and revision verification.** The pointer lives at `blobs/<slug>.yaml` once written. Confirm a revision
  landed with `navigator site document log blobs/<slug>.yaml`. It prints the revision chain, newest first, marking the
  operative row with `*`. That table is the verification step, not the exit code.
- **Login.** `navigator site login --host <host>` opens a browser-loopback flow. Its start URL is percent-encoded, so a
  literal `&` between query parameters reaches the browser intact. Where the browser will not launch, pass
  `--no-browser` to print the URL and open it by hand. The token is stored at `~/.navigator.json`.

## A court paper reads as delivered to the judge

A pleading carries only the text meant for the court. Keep drafting guidance, reviewer notes, and completion reminders
in the review record or an HTML comment (`<!-- … -->`), which never renders; never in the notation's prose. Before
presenting a pleading as ready to file:

1. Render it final, without `--preview`: `navigator notation pdf <template.md> --out <file.pdf>`. The render refuses an
   unanswered placeholder, a bracket, a stray blank, a drafting note, an instruction such as `Note to drafter:`, and an
   incomplete certificate of service, naming each line. Answer the questionnaire state it names; never fill a missing
   fact with an invented one.
2. Leave the certificate of service out until service has happened. It states the date, method, and recipients of
   service actually made, so it renders only from those recorded facts.
3. Read both the rendered pages and the extracted text (`pdftotext <file.pdf> -`). Distinguish argument about a
   verification the motion requests, which belongs, from an instruction to whoever drafts it, which does not.

## Written for the actual hearing

Write for this judge, this record, and this hearing. Before filing, the draft states:

- **The other side's best argument, at its strongest.** Steelman it, then answer it. An understated opposing argument
  is one the judge first hears from opposing counsel, at the worst moment.
- **The thirty-second story.** If the theory of the matter cannot be told in the walk to the podium, it is not ready
  to argue.
- **The record that has to exist.** Name the declaration, exhibit, or transcript page the argument stands on before
  the hearing. An argument rests on the record actually filed, not the record the draft assumes.

## Correspondence is terse

We are the experts, so we say less. Applies to email and letters to opposing counsel, vendors, and the court clerk.

- State the fact or the ask. Cite the rule by number; do not explain it.
- No argument, history, reasoning, or strategy to the other side.
- Confirm a call topic by topic: a bold one- or two-word heading, then a short paragraph. Prefer that to numbered lists.
- End with one ask and a deadline.
- Tell a vendor what to do; do not ask its view.
- Short is not vague. A notice carries what the rule requires. An ex parte notice states the relief, date, time, place,
  and whether the other side will oppose.

Example, a call confirmation:

> **Service.** We accept service of the amended complaint by email at the address on the docket. No further service is
> needed.
>
> **Schedule.** Initial disclosures are due November 6. We propose a 21-day extension for written discovery responses.
>
> **Ex parte notice.** We will apply ex parte on November 9 at 9:00 a.m. in Department 12 to continue the hearing.
> Please tell us by Friday at noon whether you will oppose. We will stipulate in writing if you agree by that time.

## Research order

Never source primary law from the open web. In order:

1. **Descrybe** for case law.
2. **Midpage** for statutes and rules.
3. **CourtListener** for dockets.

Every result still goes through source preservation before it is relied on. Found is not saved and checked.

## Court-stamped copies of a state e-filing

A California state court takes filings only through an e-filing service provider (EFSP), and the court-stamped
(conformed) copy comes back through that provider. Record each transaction, and never treat a pending filing as filed.

1. **Record the provider's email.** Save it as `.eml` and run `navigator project efiling record <email.eml>
   --submitted <asset_id>`, naming the submitted revision. An acceptance email that attaches the stamped PDF files it as
   kind `filing` with the court-filed date. An email that only says the status changed records `unknown`: log in.
2. **Download the stamped copy in the attorney's own signed-in browser.** When the email carries no PDF, the attorney
   (or an agent driving that attorney's browser session) signs in to the provider and downloads it. Never store, paste,
   or type the provider password for an agent.
3. **Pair it.** Run `navigator project efiling pair <stamped.pdf> --transaction <id> --filed-on YYYY-MM-DD`. It files
   the copy under `efilings/<id>.pdf` and links it to the submitted revision; a plain `site document upload` would not.
   Add `--judge-signed` for a proposed order returned as a received copy: the entered order is still needed.

The filed date is the date the court deems the document filed, which can precede the acceptance date.
