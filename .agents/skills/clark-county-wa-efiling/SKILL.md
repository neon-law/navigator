---
name: clark-county-wa-efiling
description: >
  File into, and read the docket of, a case in Clark County Superior Court (Vancouver, Washington): notices of
  appearance, responses, motions, declarations, and proposed orders through the Clerk's Serv-U e-file site; pulling the
  case summary (via `washington-court-records`) and Clark's OnBase images; and getting the date-stamped copy back into
  Navigator. Covers what cannot be e-filed, the 9:00-4:30 filing window, fees by E-Check, and service, which e-filing
  does not do. Use whenever a Clark County, Washington superior-court document is ready to file, a deadline depends on
  that court's docket, or the lawyer asks "where do I file this" for a Washington matter in Vancouver. Clark County,
  Nevada is a different court and skill (`clark-county-nv-efiling`).
---

# File into a Clark County, Washington superior-court case

Sources, checked 2026-11-11: the Clerk's [Electronic Filing for Superior
Court](https://clark.wa.gov/clerk/electronic-filing-superior-court) page, its Serv-U instructions (4/11/2023),
troubleshooting sheet (rev. 2025-11-06) and Q&A, and [Access to Records](https://clark.wa.gov/clerk/access-records). If
a screen or rule differs, follow the court and update this skill.

## 1. Settle where it goes

- **Court**: Clark County Superior Court, 1200 Franklin St., Vancouver, WA. Family-law case numbers carry type `3` (e.g.
  `25-3-NNNNN-D`); a `-2-` number is civil, including protection orders. The last digit is a check digit, not a county
  code (`washington-court-records` §3).
- **Portal**: **Serv-U**, `https://efile-sc.clark.wa.gov/`. A county FTP-style upload site, not Tyler and not eFileWA.
  Usernames start with `SC_`.
- **Mandatory?**: No. E-filing is open to attorneys, GALs, schools, and agencies; paper by mail or in person is still
  accepted.
- **Account**: The signing lawyer's registered Serv-U account. Registration is a form emailed to the Clerk's e-file
  registration address (file named `FirstnameLastname.pdf`) and takes **4-6 weeks**, followed by test filings. The agent
  never signs in or handles the password.
- **District Court**: Separate system and rules (`clark.wa.gov/district-court/electronic-filing`). Not covered here.

**Before relying on this route, confirm the lawyer holds a Serv-U account.** If not, register now, and file on paper
(mail or the Clerk's counter) until it is active.

## 2. What cannot be e-filed

These go on paper, by mail or in person, to the Clerk's Office:

- **Judge-signed orders.** An e-filed order is filed only as a *proposed* order. Originals go to the Clerk.
- **Ex parte orders and filings.** Present in person or mail. The ex parte fee is $30.
- Out-of-state child custody matters, habeas petitions, foreign judgments.
- Original wills and codicils (including a new probate with an original will), letters testamentary, of guardianship,
  or of administration, small estates, probate and guardianship bonds.
- Certified records for appeal, negotiable instruments, bail bonds.
- Anything handed up at a hearing or trial, and bench copies.
- Documents under a foreign government's seal, in camera submissions, interpleader or surplus-funds petitions,
  garnishment or execution affidavits, Clerk-issued writs, and aggravated-murder case documents.

## 3. Check the document before uploading

- Final: no brackets, placeholders, or draft marks. Formatted to **GR 14** (letter size, margins, caption).
- **Total page count written or typed in the upper-left corner** of each pleading.
- Signed: `s/ <Name>`, WSBA No., firm, address, phone, and email (GR 30), or wet-signed and scanned.
- **PDF or TIFF only**, black and white, **300 DPI**. Color or the wrong resolution is rejected by the e-file clerk.
- One document per file. The Clerk's instructions set no naming pattern; a working convention is
  `<case-number> <Document Title>.pdf` so the clerk can docket it from the name alone. Rename the Navigator
  `YYYY.MM.DD_name.pdf` copy for upload only.
- **Sealed family-law or guardianship material (GR 22):** the sealed cover sheet is page 1 of the sealed document, and
  the same cover sheet is e-filed again separately as a one-page public document.
- **A fee-bearing filing** (jury demand, notice of appeal, modification petition, counterclaim, third-party complaint,
  arbitration; see the [fee schedule](https://clark.wa.gov/clerk/fee-schedule)): call the Clerk's e-file line, listed on
  the e-filing page, **before** uploading. Payment is by E-Check online only; a filing without payment may be rejected.
- **A new case** is not opened by upload: email the Case Information Cover Sheet, the petition, and (family law, DV, or
  firearms) the Confidential Information Form to the Clerk's new-case e-file address on the e-filing page. The Clerk's
  office collects the fee and returns a case number; every initial pleading is then e-filed.

## 4. Drive the browser, attorney in the loop

Use the lawyer's Chrome through Claude in Chrome, in a new tab.

1. Open `https://efile-sc.clark.wa.gov/`. On the sign-in page, hand the tab to the lawyer and wait. Never type a
   username, password, or payment detail.
2. The file `Your Firm is ____.txt` shows the firm the account is registered under. It must read the Firm's name; if it
   does not, stop and tell the lawyer (the registration address updates it).
3. Copy the PDF into the session scratchpad, check its sha256 against the Navigator copy, and screenshot the file list.
4. **Stop and show the lawyer** the file name, page count, and case number. The upload *is* the filing: once dropped,
   it is filed when the Clerk's system receives it. The lawyer clicks **Upload** or drags the file in personally.
5. A file can be renamed or deleted only until the Clerk's system pulls it into OnBase. Fix a wrong name immediately
   (checkbox, then **Rename** or **Delete**); after that, call the Clerk's e-file line.

## 5. Service is separate

Uploading to Serv-U **dockets only. It serves no one.** Serve every other party under CR 5: by email only where that
party has consented in writing to electronic service, otherwise by mail (adding three days) or hand delivery. File a
certificate or declaration of service. A party under a no-contact order is served by a third party or process server
only, never by the client.

## 6. After the upload

- Each email address on the account receives an automated receipt with the **date-stamped copy attached** once the
  document loads into OnBase. Check junk mail if it does not arrive.
- From the matter repository, save the receipt with `navigator mail save <gmail-url>` and file the stamped copy:

```bash
navigator site document upload --host <host> --project <code> --kind filing --folder "Docket Filings" \
  --slug "YYYY.MM.DD_<name>.pdf" --file <stamped.pdf>
```

- Complete or move the Deadline the filing satisfied with `navigator project deadline complete`.
- Archive the receipt thread (remove `INBOX`) only after the stamped copy is filed.

## 7. Timing

**Filing hours are 9:00 a.m. to 4:30 p.m. Pacific, Monday to Friday, excluding court holidays.** A document is filed
when the Clerk's system receives it during those hours; anything received later is filed at the start of the next
business day. Upload well before 4:30 on a deadline day.

## 8. Read the docket and the case schedule

Finding the case and reading its docket is statewide: follow `washington-court-records`. What is particular to Clark
County:

- **Document images** need the Clerk's paid **OnBase** subscription (civil and domestic files from 1985; one
  subscription shared by up to five attorneys, one user at a time). Without it, order copies through the Clerk's [Access
  to Records](https://clark.wa.gov/clerk/access-records) page ($0.50 a page, $5 certified first page plus $1 a page).
- Family-law motion and Domestic Relations dockets:
  [calendars](https://clark.wa.gov/clerk/superior-court-commissioners-calendar-and-civil-motion-and-family-law-dockets)
  and [Domestic Relations dockets](https://clark.wa.gov/clerk/domestic-relations-dockets).

Record each date as a Navigator Deadline as `washington-court-records` §5 shows.
