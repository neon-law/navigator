---
name: efile-and-serve-sync
description: >
  Run a court filing end to end across tools, in order: render the paper to the court's format (WA GR 14), file it
  through the e-filing portal with the attorney-in-the-loop, draft the service email with the real signature and
  attachments, then file the evidence back into the matter. Use when a paper must be filed and served today, a clerk
  rejected a filing, or the lawyer says "refile", "file and serve", "fix the filing", or "draft the service email".
---

# File, serve, and record: keeping the pieces in sync

Workspace-level skill, run from the directory that holds the Project repositories. Matter facts stay in the matter
repository; this file names no matter. A matter's own skill supplies its court, portal, and case number.

## Order of work

1. **Read the rejection first.** The clerk's reason arrives by email from the portal vendor (search Gmail through the
   connector for the case number). A rejection with no comment still has a reason: call the clerk before the second
   attempt and ask. Record the call as a note.
2. **Render the paper to the court's format** (below). Measure the PDF; do not eyeball it.
3. **Date the paper the day it is filed and served.** The signature date and the certificate of service must match the
   real filing and service dates. Re-render if the day changes.
4. **File in the portal, stop before Submit.** The lawyer clicks Submit.
5. **Draft the service email, unsent.** Send only after the filing is in.
6. **Evidence back into the matter**, after the portal confirms. Never before.
7. **One Navigator Deadline per action item**, in the same turn.

## Washington GR 14 format (measure these)

- Letter size, one side. First page: at least 3 in blank at the top, 1 in at the bottom and sides. Later pages: 1 in.
- Nothing in the bottom 1 in, footer included. Keep line numbers out of it too.
- No attorney block, caption, or text above the 3 in line on page 1.
- Measure with `pdftotext -bbox` (words' `yMin` from the top, `yMax` from the bottom) and read the render at 80 dpi.
- The blank top is the clerk's file stamp. Say so in the portal's comment to the court, so a reviewer does not read
  it as a layout error.
- Measure Navigator's rendered pleading frame against these margins before filing. If it falls short, report it to
  Navigator by mechanism, with no PII, and render through a GR 14 frame kept git-ignored under `notes/`.
- A demand that its rule requires as a separate pleading goes in as its own document. Do not use a combined
  "appearance and request" filing code for it.

## Portal run (eFileWA, Tyler)

- Sign in; the lawyer's session usually persists in the browser profile. The browser tool opens its own tab group, so it
  cannot drive the lawyer's active tab. Say so; do not ask them to reconnect.
- Start filing > File into existing case > the court name > case number > Actions > File Into Case.
- Filing code, description, comment to the court, **Filing on Behalf of** (the client), upload the PDF. One filing per
  document. Type "eFile Only" unless the other side has registered service contacts; check **Service** for them.
- Fees step wants a payment account even at $0.00. Select the Firm's account, the responsible party, and the filing
  attorney, then **Calculate fees** and read the total.
- Stop on the Summary. After the lawyer submits, read the dashboard's filing history for the new envelope number.

## Service email (draft only, Gmail)

- Write the body as HTML, one paragraph per `<div>` or `<p>`. No hard line wraps: plain text through the CLI is wrapped
  at about 72 columns, and the recipient sees ragged lines.
- `navigator mail draft` always appends the Gmail account's **default** signature, flattened to text with no links
  or fonts, and it has no `--host` flag, so with several hosts logged in it cannot fetch the stored signature.
  `NAVIGATOR_GMAIL_SIGNATURE` is treated as plain text and shows raw tags. Do not use either for the final draft.
- Working route: create the draft with the CLI for the attachments, then rebuild the body through the Gmail connector's
  `update_draft` with the lawyer's real signature HTML, copied from one of their sent messages (the `gmail_signature`
  block). The connector drops attachments unless it is given base64 content, so re-attach the PDFs in Gmail by hand, or
  reveal them in Finder for the lawyer. Check the result with `get_draft` and look for a duplicate signature.
- Delete stale drafts for the same service, so the old version cannot go out.
- Service by email alone is valid only where the rule and the other side allow it. Confirm before relying on it, and
  keep first-class mail as the backstop when a deadline depends on it.

## Evidence back into the matter

- The portal's confirmation emails come to Gmail: submitted, then accepted (with the stamped copy) or returned.
- Save each with `navigator mail save <gmail-web-url>`, file it with `navigator mail parse`, upload the `.eml` as an
  internal exhibit, and add the pointer through a pull request. `mail/` stays git-ignored.
- Upload the stamped copy as a pleading with `navigator site document upload`, `visibility: internal`. Supersede the
  rejected pointers. Land it with the push-changes skill; the job is done at a green `cd`.

## Open gaps (report each to Navigator by mechanism, no PII)

- `navigator mail draft` has no `--host`, flattens the default signature, and cannot take an HTML signature.
- Navigator's WA pleading frame may not meet GR 14; measure it as above.
- Live Navigator reads time out; deadline rows cannot be created or checked. Note it in the handoff and retry.
