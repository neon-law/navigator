---
name: integration-fallbacks
description: >-
  Save matter work in Navigator and a predictably named file in ~/Downloads when the matter's Firm lacks an integration:
  Google Calendar or Drive switched off in Firm Integrations, no Notion page, or no Gmail connector or token. Trigger
  whenever a calendar, Drive, Notion, or Gmail step is refused, skipped, "not configured", "not provisioned", or its
  connector is missing, and before creating a personal calendar, page, folder, or mailbox to work around one. Never let
  the work drop and never route it to a tool the Firm has not set up.
---

# Integration fallbacks

Before naming a record or UI concept, follow **Find the term first** in `AGENTS.md`, then read the switches:

```bash
navigator glossary show "Firm Integrations"
```

Each Firm turns its own integrations on or off. G&S has Google Calendar and Google Drive on; other Firms may have
neither. A missing integration changes where the work lands, never whether it lands. Navigator is the system of record
in every case; a file in `~/Downloads` is the lawyer's copy to open, import, or send.

## Rules that never bend

- **Never stand in for the missing tool.** Don't create a personal Google calendar, a Notion page, a Drive folder, or
  use another mailbox to get around a switch. Only an Owner turns an integration on.
- **Never send.** Every email stays a draft for the lawyer, whatever tool holds it.
- **Never commit a fallback file.** ICS, Markdown, and `.eml` files carry client data. They live in `~/Downloads` or
  in Navigator, never in Git, a pull request, an issue, or a chat outside the matter.
- **Name every file the same way:** `~/Downloads/<code>-<YYYY-MM-DD>-<slug>.<ext>`, with the Project code copied from
  `navigator.yaml`, the date of the event or message, and a short lowercase hyphenated slug. A Deadline's ICS file keeps
  the shorter `<code>-<event>.ics` name the `matter-calendar-events` skill uses.

## 1. Detect what is missing

Check before the step, not after a failure. Read the Firm's switches from the Project's live status:

```bash
navigator project status <code> --json
```

Its `firm_integrations` object reads `{"calendar": true, "drive": false}` for the Firm that owns the Project; the plain
output prints the same as a `Firm Integrations:` line. `false` means the integration is off, and a Project without a
Firm has both off. An older server omits the object: treat the switch as unknown and ask the lawyer.

- **Notion:** `private_notion_page: not provisioned` in `navigator.yaml`, or no Notion connector in the session.
- **Gmail:** no Gmail connector in the session, and `navigator mail draft` refuses for want of a stored login.

Don't run `navigator project setup` to probe: it provisions.

## 2. Calendar off: Deadline plus ICS

Follow **When the Firm has no Project Calendar** in the `matter-calendar-events` skill. Record every date as a Navigator
Deadline and write its ICS file to `~/Downloads` in the same `navigator project deadline create ... --ics` call. Without
a Project Calendar there is no shared availability to read: ask the lawyer for open times before offering any, and say
that `calendar-conflict-check` could not run.

## 3. Notion off: Matter Note or filed transcript

Notes, call summaries, and research that belong on the Project's private Notion page go to Navigator instead.

1. **Write the body to a local Markdown file** named `~/Downloads/<code>-<YYYY-MM-DD>-<slug>.md`, such as
   `acme-auto-ny-2026-12-02-client-call-notes.md`.
2. **File it as a Matter Note**, which the client never sees:

   ```bash
   navigator project communication note --project acme-auto-ny \
     --subject "Client call notes, 2 December 2026" \
     --body-file ~/Downloads/acme-auto-ny-2026-12-02-client-call-notes.md
   ```

   Log a call itself with `navigator project communication create --channel call` and the note as its body.
3. **File a full transcript as a document**, not a note:

   ```bash
   navigator downloads file --project acme-auto-ny --kind transcript \
     ~/Downloads/acme-auto-ny-2026-12-02-client-call-transcript.md
   ```

4. **Delete the local Markdown copy** once Navigator prints the stored row or the verified digest. Keep it, and tell the
   lawyer, if the write fails.

Never paste matter notes into a Firm planning page or another matter's page to get them into Notion.

## 4. Gmail off: draft in the Firm's own mail, else an `.eml`

Use the first tool that works, and follow the `email-draft` skill for voice and structure in every case:

1. **Gmail connector**, when connected.
2. **`navigator mail draft`**, when a Gmail login is stored but the connector is absent.
3. **The Firm's own mail client**, when the Firm runs Outlook: leave an unsent draft there as the `outlook-triage`
   skill does.
4. **An `.eml` file** otherwise, written by `navigator mail draft --eml`, which needs no Gmail login:

   ```bash
   navigator mail draft --eml ~/Downloads/acme-auto-ny-2026-12-02-draft-agreement.eml \
     --to client@example.com --cc paralegal@example.com \
     --subject "Draft agreement for your review" \
     --body-file ~/Downloads/acme-auto-ny-2026-12-02-draft-agreement.txt \
     --attach ~/Downloads/acme-auto-ny-2026-12-02-agreement.pdf
   ```

   The file is UTF-8 with CRLF line ends and `X-Unsent: 1`, with each `--attach` as a MIME part. It has no `From:` and
   no signature, so the lawyer's mail client fills both. For a reply, pass `--in-reply-to` with the original
   `Message-ID` to set `In-Reply-To:` and `References:`. The command never replaces an existing file. Delete the body
   text file once the `.eml` is written.

File inbound mail the same way whatever the provider: save the `.eml` and run `navigator mail ingest <path> --project
<code>`.

## 5. Drive off: Navigator's documents tree

Without a Drive folder, every document lands in the Project's `documents/` tree. Drop it into its matter folder and run
`navigator project sync`, or file a single file with `navigator downloads file --project <code> --kind <kind> <path>`.
Don't run `navigator project drive-ingest`, and don't ask the client to share a Drive folder.

## 6. Tell the lawyer

End with one line per item: what was recorded in Navigator (Deadline id, Communication id, or document pointer), the
full path of each file in `~/Downloads`, and what the lawyer does with it (open the ICS, review and send the draft, add
the attachment). Name the integration that was off, so the lawyer can ask an Owner to switch it on.
