---
name: outlook-triage
description: >-
  Triage the lawyer's Outlook web inbox (OWA) from the workspace root, the directory that holds the Project
  repositories: read each unarchived conversation, download every message as an .eml, file it to the right Project
  repository as a Communication, record dates as Navigator Deadlines with an ICS, and leave reply-all drafts unsent. Use
  when the lawyer says "triage my Outlook", "file my email", or "process the inbox". Workspace-only: refuse to run
  inside a single Project repository.
---

# Outlook triage (workspace root)

## Run guard: do this first

Run `pwd`. Proceed only at the workspace root, the directory whose children are Project repositories (each with a
`navigator.yaml`). The Firm's companion skill names the exact path. From any other directory, including a Project
repository, stop and say: "outlook-triage runs only from the workspace root." A triage session touches many matters, and
`AGENTS.md` forbids naming one matter inside another's repository, commit, or pull request. A per-message `cd` into a
Project repository to save and file is allowed; start and finish at the workspace root.

## Rules that never bend

- **Draft-only.** Never click Send, Schedule send, or Send later. Every reply is an unsent draft. A `Discard` of an
  unsent draft needs the lawyer's yes first.
- **Outlook goes through the browser** (Claude in Chrome, one tab you create). The Gmail connector does not read
  Outlook. The browser `javascript_tool` is blocked on this site, so read with `find`, `read_page`, `get_page_text`.
- **No matter facts outside Navigator.** Never put a client name, Project code, or email address in a commit, issue,
  pull request, or this skill. Saved mail stays under each repository's git-ignored `mail/`.
- Report outcomes faithfully. A refused filing is unfinished work, not a footnote.

## Loop, one conversation at a time (oldest first, or as the lawyer orders)

1. **Open** the conversation. Use `get_page_text` to read it and decide which Project each message belongs to. Match
   From/To/Cc to each Project's Person and Entity emails in `<workspace>/*/navigator.yaml` and `seeds/`. Case numbers in
   subjects help. If it is unclear or two matters fit, ask the lawyer. Skip invitations and quarantine digests unless
   they carry a date to record.
2. **Download each message as `.eml`, not the conversation row.**
   - The conversation row's right-click `Download as EML` returns only one message, often an old one already filed.
     Expand the conversation and download each message from its own card.
   - On a card: `...` (More items) → `Download` → `Download as EML`. A collapsed card must be expanded first (click
     the chevron in the list, or "Show message history").
   - Menu refs change every time. Run `find` for "Download" and then "Download as EML" after each open; do not reuse
     refs. The file lands in `~/Downloads` as `<Subject>.eml`, with `(1)`, `(2)` suffixes for repeats.
3. **Verify** each download with `python3 -I`: print From, To, Date, Subject, Message-ID, attachment names, and
   sha256. Confirm it is the message you meant, not a duplicate of one already filed in that repository.
4. **Save and file** from inside that Project's repository:
   - `cp` to `mail/YYYY.MM.DD_<short-subject>.eml`, dated by the message's send date. Add the time or sender when
     several messages share a day.
   - Check `navigator mail parse --help`, then run `navigator mail parse mail/<file>.eml --project <code> --host
     <host>` (code and host from `navigator.yaml`). Success reads `Filed ... as a Communication`.
   - **One message cannot be filed under two Projects.** The second is refused with "source reference already belongs
     to another Project." Keep the `.eml` locally in the second repository, tell the lawyer, and offer to report the
     mechanism to Navigator, never the matters.
5. **Dates.** For each hearing, deadline, or meeting a message states:
   - `navigator project deadline list --project <code> --host <host>` first. Update an existing row rather than adding
     a second.
   - A date read from the tribunal's own notice is stated plainly; a date someone reported gets an `UNVERIFIED:` title
     and an authority line naming who reported it.
   - `navigator project deadline create ... --ics notes/YYYY.MM.DD_<name>.ics --replay-key <stable-key>`. Use explicit
     offsets, such as `-05:00` for New York in winter.
   - Meeting invitations for matters with no repository get an ICS in the scratchpad, not in any repository.
   - The Google Calendar connector may return "Insufficient scope". Then it can neither read nor create events; say
     so and leave the ICS for the lawyer to import. Never invent a calendar.
   - **Calendar sync gate.** `navigator project deadline sync` fails with `runtime_not_configured` when the Project's
     Firm has no Google Workspace calendar owner, and with `provider_unauthorized` when the delegation lacks the
     calendar scope. Either is a configuration state, not a bug: do not retry, tell the lawyer once that the deadline
     stays in Navigator only, and do not create a per-deadline ICS to work around it. The Firm's companion skill says
     which Firms are in that state. Use `navigator project deadline ics` for a one-off export when the lawyer asks.
6. **Reply drafts.** Open the latest message in the conversation, click `Reply all`, confirm To/Cc in a screenshot,
   click in the body above the signature, and `type` the text. Outlook autosaves. Do not add a sign-off; the signature
   is the close. If a heading needs bold, select it and press cmd+b one heading at a time, waiting a second between
   presses, and verify. When the lawyer gives a booking link, put it in the text. If a message went to several families
   or clients, ask before replying to all of them on one message.
7. **Archive nothing** unless the lawyer asks, and only after every filing above reads back.

## Triaging the Drafts folder

Open each draft and read it with `get_page_text`. Report subject, recipients, whether the body is empty, and whether its
dates have passed. Recommend keep or delete; never delete without a yes. To delete, open the draft and use the trash
icon in its toolbar, then confirm `OK` in the dialog. Do not press the adjacent Send button.

## Gotchas

- The reading pane layout shifts; screenshot before every coordinate click, and prefer `find` refs.
- `navigate` away from an open compose can trigger a "Leave site?" guard. Do not force it; let the draft autosave or
  deal with the draft first.
- A forwarded message can hold a tribunal's notice in its body. File the forward, which carries the PDF.
- Attachments are legal files; they travel through `navigator mail parse` into Navigator and never into Git.
- At the end, list what is filed, what was refused, what remains, and every draft left unsent.
