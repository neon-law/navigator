---
name: clark-county-nv-efiling
description: >
  File a document into an existing case in the Eighth Judicial District Court (Clark County, Nevada) through Odyssey
  eFileNV, family or civil division: notices of entry, notices of appearance, answers, motions, oppositions, and
  certificates of service. Covers which court and portal take the filing, the values each field wants, the
  attorney-in-the-loop browser run that fills the envelope up to Summary, filing the accepted, stamped copy back into
  Navigator, and pulling down a file-stamped copy of something already on the docket. Use whenever a Clark County
  district-court document is ready to file, the lawyer asks "where do I file this", "file it with the court", or "e-file
  this", or asks "do we have the stamped copy", "pull down the docket", or "get the filed version".
---

# File into an existing Clark County district-court case

## 1. Settle where it goes

- **Court**: Eighth Judicial District Court, Clark County, Nevada. A `D-` case number is the Family Division; an `A-`
  case number is Civil.
- **Portal**: Odyssey eFileNV (Tyler), `https://nevada.tylertech.cloud/OfsWeb`. E-filing is mandatory for attorneys in
  this court.
- **Location label**: Family: "Clark District Family Domestic". Civil: the Clark District civil location the portal
  lists.
- **Account**: The signing lawyer's own eFileNV account. The agent never signs in, never sees the password, never stores
  it.

**Not everything is e-filed by counsel.** A stipulation and order goes to the department's order inbox by email for the
judge's signature; chambers files it. Counsel then e-files the **Notice of Entry** of that order. A proposed order alone
goes the same way. Everything else counsel files goes through eFileNV.

## 2. Check the document before the envelope

- It is final: no brackets, no `[INSERT]`, no unfilled placeholder, no draft marker.
- It is a PDF, text-searchable, in the `YYYY.MM.DD_name.pdf` form. Word goes up only where the court asks for Word.
- **The date matches the day it will be submitted.** A certificate of service that says "on the Nth day … I served …
  through the court's electronic filing and service system" is only true if the envelope is submitted that day. Redate
  the signature line(s) and the certificate before filing on any later day. Never redate the date an order was
  *entered*.
- Exhibits ride inside the one PDF unless the court's code calls for separate attachments.

## 3. The values

- **Filing Type**: `EFileAndServe`. `EFile` dockets without serving, which falsifies a certificate of service. `Serve`
  serves without docketing.
- **Filing Code**: Search the code's short form. Family codes end in `(FAM)`. Confirmed in the portal: `Notice of
  Appearance - NOA (FAM)` and `Notice of Entry of Order - NEOJ (FAM)`. If the short form is not found, search the long
  name and pick the closest family code.
- **Filing Description**: The document's full title, as captioned.
- **Filing on Behalf of**: The party the lawyer represents.
- **Lead Document**: The PDF. Security: the public default, unless the document is sealed or confidential by rule.
- **Service Contacts**: Every opposing counsel and their assistants on the case's e-service list, checked. Compare with
  the last court notice of service in the matter's mail.
- **Payment Account / Filing Attorney**: The lawyer's saved account; the lawyer as Filing Attorney.

A family-division notice (NOA, NEOJ) carries a $0.00 court fee plus a $3.50 e-file fee (October and November 2026); the
Fees panel shows the real figure before Submit. The card is held, and charged only on acceptance. Party Responsible for
Fees is the client party; the saved firm card is the Payment Account.

## 4. Drive the browser, attorney in the loop

Use the attorney's real Chrome through Claude in Chrome. Follow the matter repository's `browser-handoff` skill for
getting the browser and for what to do when the tools are missing.

1. Open a **new tab** at the portal. If it shows a sign-in page, hand the tab to the lawyer and wait. Never type a
   password, a code, or payment details. The portal often lands straight on the Filer Dashboard because the lawyer's
   session is still live.
2. **File Into Existing Case** → pick the **Location** first (the case-number box appears only after it), then search
   the case number → confirm the caption → **Actions** → **File Into Case**. The Location list is a custom widget, not a
   `<select>`: open it, type a unique word ("Domestic") to filter it to one row, then click that row. Clicking a row in
   the unfiltered list has selected the row below it, so zoom and confirm the label before searching.
3. Fill the Filings panel from §3. **Filing on Behalf of** opens a party list on click. The Lead Document input takes
   a direct file upload, but the upload tool reads only session-readable paths: copy the PDF from `~/Downloads` into the
   session scratchpad first, and check the sha256 matches. Security defaults to "Public Filed Document". **Save
   Changes**. **Set Filing Code before Filing Type, then re-read Filing Type.** On 2026-11-18 Filing Type was set to
   EFileAndServe first and silently fell back to the default `EFile` when the code was chosen; the envelope filed
   without serving. A served envelope shows a **Service Contacts** panel after Save Changes, and Summary lists the
   contacts. No Service Contacts panel means it will not serve: stop and fix the type. If it already went out as EFile,
   send a second envelope with Filing Type `Serve` (code becomes "Service Only", $0.00) carrying the same PDF.
4. Confirm the service contacts are checked; do not add or remove a contact without asking.
5. In Fees, select the saved account and the Filing Attorney only. **Save Changes**.
6. Click **Summary**, screenshot it, and show the lawyer: case, code, description, document and page count, service
   contacts, fee.
7. **Stop there.** The lawyer clicks **Submit** personally. Submit files a court paper, serves opposing counsel, and
   charges a card; the agent never clicks it, nor **Pay**, **Resubmit**, **Cancel Envelope**, or anything on an account,
   payment, or contact settings page. **Save as Draft** is allowed when the lawyer wants to stop midway.

If a screen label differs from these, follow the screen and update this skill.

## 5. After Submit

Tyler sends three emails from the `efilingmail.tylertech.cloud` domain, each carrying the envelope number in its subject
(search Gmail for `from:efilingmail.tylertech.cloud <envelope-number>`):

- **Notification of Service**, about a minute after Submit: filing code, the service contacts, and a 30-day link to
  the served document.
- **Filing Submitted**, about a minute after Submit: court, submitted time, filing type and code, filed-by, page count,
  and **Fee Details**: the total and a link to the envelope receipt,
  `https://nevada.tylertech.cloud/OfsWeb/FileAndServeModule/Envelope/ViewPrintableEnvelope?Id=<envelope>`. It says the
  card is only held until acceptance.
- **Filing Accepted / Filing Returned**, minutes (auto-review) to days later: accepted time, clerk comments, and the
  file-stamped copy's link (548 days). See §7 to download it.

Until acceptance the envelope reads **Submitting** and every eService row reads **Not Sent**; that is normal, not a
failure.

**The receipt** is the envelope's printable page, the link in Filing Submitted. It shows the fees, Transaction Id, Order
Id (`0<envelope>-0`), and Transaction Response (`Authorized` while held). Tyler emails no separate receipt. To keep a
PDF of it, open the page in the lawyer's signed-in browser, clone `#page` without the elements the live page hides
(their computed `display` is `none`; otherwise the portal's help panels print on page 1), keep only the
`tylertech.cloud` stylesheet links, save that as HTML, and render it with headless Chrome. Read the PDF before filing
it. File the receipt in the matter's Costs folder:

```bash
navigator site document upload --host <host> --project <code> --kind receipt --visibility internal \
  --folder "Costs" --slug "YYYY.MM.DD_efiling-receipt-envelope-<envelope>.pdf" \
  --description "eFileNV envelope <envelope> receipt: <filing code>, e-file fee \$<amount>, transaction <id>" \
  --file <receipt.pdf>
```

Check the pointer's sha256 against the PDF, then commit the pointer. Every matter expense (court and e-file fees,
depositions, transcripts, certified mail, process service) gets a receipt this way.

From the matter repository:

```bash
navigator mail save <gmail-url-of-Filing-Accepted>
navigator project efiling record "mail/<YYYY.MM.DD_subject>.eml" \
  --efsp eFileNV --court "Eighth Judicial District Court" --case-number <case-number> \
  --filed-on <YYYY-MM-DD> --host <host>
```

As of navigator 26.11.10, `efiling record` refuses a Tyler Filing Accepted email with "the email names no e-filing
transaction id": it does not read Tyler's "Envelope Number". When that happens, keep the saved `.eml` files, say so, and
do not retry with invented flags.

If the stamped PDF was downloaded by hand (§7), file it with `navigator project efiling pair --transaction
<envelope-number> <stamped.pdf> …`. `pair` refuses with "transaction names no submitted version" unless the submitted
draft was uploaded to Navigator first; pass `--submitted <asset id>` if it was, and otherwise use the plain upload in
§7. Then complete or move the Deadline the filing satisfied with `navigator project deadline complete`.

**Archive the emails last.** Once all three are saved with `navigator mail save` (and the filing is recorded, or the gap
reported), archive each thread by removing its `INBOX` label: the Gmail connector's `unlabel_thread` with `labelIds:
["INBOX"]`. Archive only; never trash or delete. Do not archive a Filing Returned until it is fixed and resubmitted.
Confirm with a Gmail search such as `from:efilingmail.tylertech.cloud <envelope-number> in:inbox`, which should return
nothing.

A **Filing Returned** is not a filing. Read the clerk's comment, correct, resubmit, and keep the deadline open.

## 6. Timing

The portal is open around the clock. A filing submitted on a weekend or a court holiday is generally file-stamped the
next court day; check NEFCR and the court's holiday list when a deadline is close.

## 7. Pull down a file-stamped copy already on the docket

Use this when a filing was accepted but the matter does not hold its stamped copy yet. To check, compare `blobs/Docket
Filings/` with the Tyler and court emails.

1. **Find the link in Gmail.** Run `from:efilingmail.tylertech.cloud "Filing Accepted" <case-number>`, or search
   `"notice of entry"`, `"<filing code>"`, or the envelope number. Read the thread with `get_thread` and `messageFormat:
   FULL_CONTENT`. The plain-text body drops the href. The **File Stamped Copy** row's "Download Document" link has the
   form `https://nevada.tylertech.cloud/ViewDocuments.aspx?FID=<guid>` and is good for 548 days after acceptance. For
   the other side's filings, the **Notification of Service** email holds a 30-day link to the served copy. An order
   chambers enters arrives from the `clarkcountycourts.us` domain ("Proposed Order Accepted").
2. **Open the link in a new tab of the lawyer's signed-in Chrome.** On 2026-11-18 it downloaded the PDF straight to
   `~/Downloads` under the lead document's filename. It asked for no email, and the tab showed no page, so a screenshot
   fails. Run `ls -lt ~/Downloads | head` instead. If the page does ask for the filer's email, that is personal data
   entered into a form, so ask the lawyer first.
3. **Check it is the stamped copy.** `pdftotext -l 1 <pdf> - | head` must begin with `Electronically Filed`, the
   date and time, and the Clerk of the Court. `pdfinfo` must show the page count from the Filing Accepted email. Record
   `shasum -a 256`.
4. **File it in Navigator** on a topic branch cut from `origin/main`:

   ```bash
   navigator site document upload --host <host> --project <code> --kind filing --folder "Docket Filings" \
     --slug "YYYY.MM.DD_<name>.pdf" \
     --description "<title>, file-stamped <date time>, eFileNV envelope <envelope>" \
     --file <stamped.pdf>
   ```

   Date the slug with the file-stamp date, drop `-final` from the name, and use `--kind filing`. The default visibility,
   `internal`, is correct. Check that the pointer's sha256 matches the PDF. Upload from `~/Downloads`, or delete any
   staging copy under `documents/` afterwards, because `navigator project sync` re-uploads a stray file it finds there.
5. Gate the change, then commit, push, and open the PR as the matter's `push-changes` skill says. Complete the
   Deadline the filing satisfied, if it is still open.

What this does not yet cover: the full docket, including entries the Firm was never served with. Nothing here has been
verified against the court's own public case-records portal. When the lawyer reads the docket there, add the steps to
this section and do not guess the URL.
