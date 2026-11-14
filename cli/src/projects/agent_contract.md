# Working in a Navigator Project repository

## Critical: email is draft-only

**Every email must remain a draft. An agent never sends, replies to, or forwards email.** The lawyer reviews each draft
and sends it personally. A request to send, reply, or forward means prepare a draft for lawyer review instead.

This repository is the source of one Navigator Project, named in `navigator.yaml`. Git is its history: every change to
the matter's pages, Drafts, and document pointers is a reviewed commit. Declare the change in YAML or Markdown; do not
write application code. When a declaration cannot say what the matter needs, open a Linear issue for a new Section or
gate rule rather than working around it.

## The Firm

Name the Firm by its role: "the Firm". Do not spell a Firm's legal name in this file, in a skill, in `README.md`, or in
a Draft. Where a letter must print that name — the letterhead, the From line, or the signature block — write
`{{firm.legal_name}}`. Navigator fills it from the Firm behind the Brand declared as `project.brand`. A sentence that
states a fact about a named entity, such as a conversion, a shareholding, or a services agreement, keeps that name.

## Find the term first

Before naming a table, field, state, or UI concept, run `navigator glossary list` to find a term and its plain-language
description, then `navigator glossary show "<Term>"` for its full definition. Use the glossary's term when one fits; if
none fits, add a term under `docs/glossary/` in the same Navigator change rather than inventing a synonym.

## The loop

1. `navigator project sync` downloads the Project documents visible to you.
2. Edit `navigator.yaml`, `drafts/`, or `blobs/` pointers.
3. `hudson preview` renders the Matter Pages from the working tree.
4. `navigator project gate` before every commit; it is the required check.
5. Open a pull request. Merging it publishes.

For architecture or process questions, convene the `council` skill; for how a client will read a page or letter,
`client-council`; for the legal substance, `legal-council`. Review a finished memo or letter with `analyze-memo`
(`.agents/skills/analyze-memo/SKILL.md`); report findings before applying corrections on the lawyer's go-ahead. Keep
each answer terse.

`navigator project onboard` and `navigator project offboard` draft or open the engagement and offboarding letters from
`navigator.yaml`. `navigator project repository sync-skills` restores missing or drifted canonical Agent Skills and this
`AGENTS.md`; keep custom skills beside them.

## People and Entities in matter work

When a message, Docket Entry, order, or Notation Answer identifies a Person or Entity that Navigator does not already
record, ask the lawyer whether to record them and whether the record belongs to the Project, the lawyer, or the Firm.
Ask when the name surfaces, including for opposing counsel, witnesses, guardians, and related parties.

For approved Project records, use `navigator project reference add` for a verified party and `navigator project adverse
add` when the lawyer confirms adversity. Record the lawyer's decision and authorship. If the approved scope has no
available CLI path, or a Person cannot be recorded because required information is missing, tell the lawyer and record
the gap in the private matter handoff.

## Branches

* `main` is the only long-lived branch. Change it only through a pull request; never push to it directly.
* Pull requests are squash-merged, and the head branch is deleted on merge.
* Open pull requests ready for review by default; use a draft only when deliberately holding work for more changes.
  On GitHub, respect App-owned automatic squash auto-merge and confirm `autoMergeRequest` is non-null or the pull
  request is already merged. If automation does not arm it, inspect and report the blocker. On another forge, use its
  equivalent ready-for-review and merge-when-checks-pass controls, and verify the pull-request state and merge policy.
* Before merging, review every changed document pointer (`blobs/**/*.yaml`): retain `visibility: internal` unless
  the client is meant to read that document. Treat every change to `visibility: client` as a deliberate client-release
  decision. Review every change to a `client:` lens under `pages:`; it is what the client sees. For each Deadline,
  choose deliberately whether it is internal or client-visible.
* "Pull changes" means rebase onto refreshed `origin/main` from a clean tree. Commit work in progress; never stash,
  merge `origin/main`, or force-push `main`. Follow `.agents/skills/pull-changes/SKILL.md` for conflict handling,
  validation, and `--force-with-lease`. A host-side sync tool is allowed only when the sandbox blocks rebase; report it.
* "Push changes", "push this", "ship it", and "land it" mean signed-rebase onto the latest `origin/main`, gate, sign,
  push a topic branch, open a ready PR against `main`, and follow its checks and auto-merge through merge. The job ends
  when the pull request is merged and this checkout is current. Follow `.agents/skills/push-changes/SKILL.md`.
* The required check is `ci`, which runs `navigator project gate`.
* The forge enforces branch and merge settings; client-visibility decisions remain part of human review. A pre-commit
  hook is a convenience only.

## Folders

* `navigator.yaml` names the Project and declares its Matter Pages under `pages:`: each page has a slug, a kind, and
  the Sections each lens (`lawyer`, `client`) shows. Navigator renders them at `/app/projects/<code>/<slug>`.
* `blobs/` holds one committed Pointer per document: `blobs/<slug>.yaml` names the document's live
  revision. The Blob is the revision's bytes in Navigator-managed storage.
  * `blobs/onboarding/` and `blobs/offboarding/` hold the pointers for the letters that open and close the Project. A
    signed copy is named `<name>_signed.pdf` and its pointer carries `docusign_envelope_id`.
  * `blobs/invoices/` holds the pointers for invoices named `INV-<digits>.<ext>`; every pointer carries
    `xero_invoice_id`.
  * A pointer's optional `folder:` names the Matter Folder its bytes belong in; without it, the folder follows from
    `kind`.
* `documents/` is git-ignored. It holds the bytes `navigator project sync` pulls down or uploads, organised the way a
  case-management system organises a matter. `navigator project sync` creates every folder, even when it is empty:
  * `Client Production/` holds what the client supplied, such as exhibits (`kind: exhibit`). Its two fixed subfolders
    nest below it: `Agreements/` and `Screenshots/` — a pointer's `folder:` names them, or `navigator project
    migrate-folders` files a pointer there; no `kind` defaults to either.
  * `Correspondence (Served)/` holds letters served or received (`kind: letter`).
  * `Discovery (Served)/` holds discovery actually served or received.
  * `Docket Filings/` holds court paper: pleadings, filings, and docket ingest (`kind: pleading`, `filing`).
  * `Court Rules/` holds the court's local rules and standing orders.
  * `Engagement/` holds the letters that open and close the engagement (`kind: onboarding`).
  * `Work Product/` holds drafts, research, and every other kind. Its seven fixed subfolders each carry the suffix so
    they read unambiguously out of context and never collide with a served twin: `Client Calls (Work Product)/` (call
    notes and transcripts, `kind: transcript`), `Correspondence (Work Product)/` (letters the firm drafted but did not
    serve), `Discovery (Work Product)/` (discovery in preparation), `Motions (Work Product)/` (one folder per motion),
    `Pleadings (Work Product)/` (pleadings in draft), `Research (Work Product)/` (memos, `kind: memo`), and
    `Stipulations (Work Product)/`.
  * `onboarding/`, `offboarding/`, `invoices/`, `cases/`, and `rules/` keep their bytes at the pointer's own path.

  The slug is the path below the matter folder: `documents/Docket Filings/pleadings/motion.pdf` for
  `blobs/pleadings/motion.pdf.yaml`. A file dropped into a matter folder is uploaded on the next sync with that folder's
  kind, and a lawyer-made subfolder inside one is part of the slug. Never commit anything under `documents/`. Run
  `navigator project repository migrate-folders` to reconcile `Discovery/` with `Discovery (Served)/`.
* Every document's filename follows the `YYYY.MM.DD_name.ext` convention (rule `C004`): a dotted calendar date, one
  underscore, and a lowercase hyphenated name — `2026.10.12_complaint-annulment.pdf`. Name a file this way when you
  stage or create one; an upload without an explicit `--slug` proposes the convention, dated by the `matter:` block's
  `filed_on` (a date the file's own name already carries wins). The gate refuses any other name, except the shipped
  docket-ingest names under `Docket Filings/`, the `INV-<digits>` invoices, an Authority sidecar, the `_signed` suffix
  on a signed copy, Markdown work product (a memo or draft is editable source, not a filed document), and a file inside
  a date-named folder such as `exhibits/2026-09-05/` — the folder already dates it.
* `drafts/` holds Drafts, the notation blueprints, directly under it; `templates/` is an alias the gate warns about.
  Memo notations use `kind: memo`. Use placeholders for client data and matter facts; answers and work product stay in
  Navigator. A Notation is a database record, never a file here.
* `seeds/` holds seed data that `navigator site import` creates or updates.
* `notes/` is git-ignored. Write call notes there for `navigator project communication create --body-file`.
* `mail/` is git-ignored. Saved mail lives there: `hudson mail save <message-url>` writes a message's
  `YYYY.MM.DD_<subject>.eml` copy, and email bodies kept on disk for `hudson mail draft --body-file` belong there too.
  Saved mail is a matter fact, never Git; the gate refuses anything under `mail/`.
* Handoff note: work that may outlast the session keeps its state in `notes/HANDOFF-<topic>.md.local`. The gate does
  not read it, and no matter fact leaves the repository. Follow `.agents/skills/working-notes/SKILL.md`.
  * Write it before a session ends with work in flight, near compaction, or when the lawyer asks. Rewrite it when the
    plan changes; never append a contradiction.
  * Sections, in order: Plan (with the rule behind each computed date), Files (paths, render command, open
    placeholders), Drafts waiting (unsent drafts and when each goes), Deadlines (Navigator rows), Reminders (scheduled
    messages; flag stale ones), Open items.
  * One fact per line, absolute dates with a time zone. Cite message ids, replay keys, and PR numbers; do not retell.
  * When the work lands, log what belongs in the record as a Navigator Communication, then delete the note.

## Final documents only

Every document this repository produces for a client, a court, a counterparty, or a signer is final.

* Never deliver a fill-in-the-blank document: no blank lines, brackets, `[INSERT]` markers, unfilled `{{placeholders}}`,
  TODOs, or notes to the drafter or the reader.

* A `{{placeholder}}` belongs only in a Draft; Navigator fills it from answers when it renders the document.
* When a fact is missing, ask for it through a notation questionnaire and render again. Never invent it.
* Signature, initial, date-of-signing, and notary blocks are the only spaces a person completes, and only at signing.
* Read the rendered pages before calling a document ready to send, sign, or file.

## Tools

Use available MCP servers and the `navigator` CLI, preferring them whenever they provide the needed operation. Never
substitute browser or native-app automation for a server that is available.

* **Navigator CLI** — the system of record.
  Document bytes, archived authorities, and answers go through Navigator and never into Git. Preview a memo with `hudson
  notation preview drafts/<code>.md`. A direct upload from this checkout writes only its pointer under `blobs/`; review
  and commit that `.yaml` file.
* **Notion** — writing: handbooks, guides, and intake notes.
* **Gmail** — client instructions and correspondence. Read messages and create drafts only; never send, reply to, or
  forward an email.
* **Google Calendar** — meetings and deadlines.
* **Google Drive** — meeting notes, transcripts, and large document intake.
* **Slack** — private channel for updates.
* **Linear** — CLI feedback, named by mechanism and never by matter. Remove personal and matter identifiers before
  filing: write `<client>`, `<claim number>`, `<email>`, `<phone>`, `<address>`, `<identifier>` in their place, and
  never quote matter text. Run `navigator project feedback-check --all-projects` on the draft title and body; add
  `--term` for private names the store does not know. The check resolves all Projects visible through your firm-side
  participation and their attached Person and Entity identifiers through the site API with your `site login` session.
  The draft stays local. A failed read or finding blocks filing. Show the checked text to the lawyer before filing.
* **CourtListener** — dockets, filings, parties, and judges.
* **Midpage** — statutes and rules.
* **Descrybe** — case law.
* **Exa** — the open web, never primary law.

The last four leave the firm. Keep client identities and matter facts out of those queries.

For a client email:

* Run `hudson mail draft --body-file` to create an unsent draft of the substantive message body. Do not paste
  a signature from another message: the CLI adds the attorney's stored signature — the deployment's row for the sending
  Gmail account when reachable, else the `NAVIGATOR_GMAIL_SIGNATURE` fallback it announces, else the Gmail account's
  default signature — including its confidentiality notice, exactly once. End the body without a sign-off line; the
  signature is the close. Do not invent signature text.
* If the CLI reports an embedded signature, stop for attorney review. Preserve quoted history rather than stripping it
  to force the draft through.
* The attorney reviews the complete draft in Gmail, confirms one signature and sends it personally. Do not insert
  another signature during review.
* A `--draft-id` update replaces the complete draft. Supply every recipient, the thread and every attachment that must
  remain; signature handling preserves those supplied headers and attachment bytes.
* To file a received message into the matter's Communication log, run `hudson mail parse <gmail-web-url>` (the
  URL copied from the browser), `hudson mail parse <message.eml>`, or `hudson mail parse <document.pdf>` — the last
  uploads through the document lane. Filing is a record write only; it never sends or archives in Gmail.
* To keep a Gmail message's .eml copy in this checkout, run `hudson mail save <gmail-web-url>`; it writes
  `mail/YYYY.MM.DD_<subject>.eml` and refuses a filename that drops the send-date prefix.

## Feedback

When Navigator's CLI is missing or wrong, open a Linear issue on the Lawyers team rather than documenting a CLI
workaround here.
