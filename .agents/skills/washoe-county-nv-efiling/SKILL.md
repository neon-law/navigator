---
name: washoe-county-nv-efiling
description: >
  Files into, and reads the docket of, a case in the Second Judicial District Court (Washoe County, Reno, Nevada)
  through the court's own eFlex system: Family Division divorce and post-decree motions to modify child support, spousal
  support, custody, or visitation, and civil filings. Covers the case number, the eFlex account, WDCR 10 format,
  reopening fees, the attorney-in-the-loop browser run that stops before Submit, NEF service, the 11:59 p.m. rule, and
  the stamped copy and deadlines in Navigator. Use whenever a Washoe County district-court document is ready to file, or
  the lawyer asks "where do I file this" for a Reno matter. Clark County, Nevada is `clark-county-nv-efiling`.
---

# File into a Washoe County district-court case

Sources, checked 2026-11-11: the court's [Electronic Filing](https://www.washoecourts.com/EFiling) and [eFlex
FAQ](https://www.washoecourts.com/Efiling/FAQs) pages, the [Filing
Office](https://www.washoecourts.com/FilingOffice/FilingRules) page and [Resource Center
FAQ](https://www.washoecourts.com/FilingOffice/FAQs), the [Fee
Schedule](https://www.washoecourts.com/FilingOffice/FeeSchedule) (effective 2025-09-30), the Code List (rev.
2026-08-21), the eFlex [User Guide][guide] (Tybera, rev. 2016-04-18, old), [Administrative
Orders](https://www.washoecourts.com/Main/AdminOrders) 2026-11 to -13,
[WDCR](https://www.leg.state.nv.us/courtrules/seconddcr.html), and
[NEFCR](https://www.leg.state.nv.us/courtrules/NEFCR.html). If a screen or rule differs, follow the court and update
this skill.

[guide]: <https://www.washoecourts.com/OtherDocs/eFiling/Filer%20Interface%20User%20Guide%2001172014%20rev%20041816.pdf>

## 1. Settle where it goes

- **Court:** Second Judicial District Court, Washoe County, Nevada. Family Division at 1 S. Sierra St., general
  jurisdiction at 75 Court St., Reno ([EFiling](https://www.washoecourts.com/EFiling)).
- **Case number:** prefix, two-digit year, five digits, e.g. `DV24-00000`, `FV24-00000`, `CV24-00000`, plus a
  `Dept. No.` (WDCR 10(11)(d); the rule's sample caption uses `CV99-00000`). The court's search index shows `DV` on
  divorce cases, `FV` on other family cases (custody, visitation), `CV` on civil. **Unconfirmed:** the court publishes
  no prefix legend; read the prefix off the case's own caption, never infer the division from it alone.
- **Portal:** **eFlex**, `https://wceflex.washoecourts.com`. The court's own system built by Tybera, **not Tyler and
  not eFileNV** (User Guide footer). The court moved to a new case management system on 2026-05-01 to 05-04, and eFlex
  stayed the filing system (AO 2026-11, AO 2026-13).
- **Mandatory?** Yes. "Electronic filing is mandatory for all cases," Administrative Order 2018-12
  ([EFiling](https://www.washoecourts.com/EFiling)); NEFCR 4(a) requires every Nevada attorney to e-file.
- **Account:** the signing lawyer's own. The court offers **no firm accounts**; every attorney listed on a case must be
  an e-filer ([FAQ](https://www.washoecourts.com/Efiling/FAQs)). Standard account is free and sees only the lawyer's own
  cases; Subscription is $300 a year and sees public documents in all public cases. Sign-up is an **ink-signed** User
  Agreement plus "Request an Account" on eFlex; approval takes up to 3 business days, so confirm the lawyer holds an
  account before relying on this route. The agent never signs in, never sees the password, never stores it.

## 2. What counsel does not e-file the ordinary way

- **Proposed orders.** WDCR 10(10)(c)(5) lets the clerk strike "an unsigned order that is not identified as a
  proposed order", so its title must say it is proposed. The Family Division's Request for Submission form (code `3860`,
  [form](https://www.washoecourts.com/Forms/FamilyCommon/Request%20for%20Submission%20and%20Index%20of%20Exhibits.pdf),
  rev. 2026-03-19) lists the proposed order as Exhibit 1. The 2016 User Guide says proposed orders go up as **Word**
  files, so the judge can edit them, and that "Accepted, Presented to Judge" does not mean signed. **Unconfirmed:**
  whether eFlex still wants Word for a proposed order (the FAQ names PDF only) and whether a department wants it emailed
  instead. Ask the department's clerk before the first one.
- **Stipulations** need every party's signature, scanned from paper with handwritten signatures (WDCR 10(5)(b)(2)), or
  an e-signature (NEFCR 12(c)). No Affirmation goes in a proposed order (WDCR 10(7)(a)).
- **Writs of execution and attachment** are mailed with the fee ([FAQ](https://www.washoecourts.com/Efiling/FAQs)).
- **Ordered payments other than filing, jury, and peremptory-challenge fees** (appeal bonds, cost bonds) are paid at
  the Resource Center or by mail, not in eFlex (AO 2026-13).
- **Ex parte orders** go only to the assigned department (WDCR 17); in Family, notice gives the other side 24 hours
  to oppose unless an exception applies (WDCR 43).

## 3. Check the document before the submission

- Final, no placeholders, **dated the day it will be submitted**, certificate of service included.
- **WDCR 10 format:** 8.5 x 11, numbered lines, double spaced, 12-point, pages numbered at the bottom, portrait, a PDF,
  right side up. **Top left from line 1: the document code** from the Code List, then party, bar number, and the
  attorney's mailing address, email, and phone (WDCR 10(11)(a)). Codes include `2520` Notice of Appearance, `2540`
  Notice of Entry of Ord, `2340` Mtn to Modify Child Support, `2350` Mtn Modify Custody/Visit, `2355` Mtn to Modify
  Decree, `2360` Mtn Modify Spousal, `3860` Request for Submission.
- **Family caption** reads "IN THE FAMILY DIVISION / OF THE SECOND JUDICIAL DISTRICT COURT OF THE STATE OF NEVADA / IN
  AND FOR THE COUNTY OF WASHOE"; a paper where custody is at issue says so in its title (WDCR 38).
- **Affirmation** (no personal information, or the law requiring it) is the last page before exhibits, or the last
  sentence before the signature; missing it lets the clerk strike the paper (WDCR 10(7), 10(10)(c)(2)).
- **Exhibits are separate PDFs**, never batched into the motion, after an Index of Exhibits giving number, description,
  and page count (WDCR 10(9)). This is the opposite of Clark County.
- One case per paper; motion, opposition, and reply each separate (WDCR 10(2)-(3)). Limits: 20 MB a document,
  100 MB a submission; split anything larger ([FAQ](https://www.washoecourts.com/Efiling/FAQs)).
- **Motion to modify support:** attach the court-approved Financial Declaration (WDCR 40(2)). **Custody at issue
  post-judgment:** the motion and the opposition carry the UCCJEA sworn statement (WDCR 39). Points and authorities and
  affidavits go with every motion (WDCR 12(1)).
- **Fees** ([schedule](https://www.washoecourts.com/FilingOffice/FeeSchedule)); the Review and Approve page shows the
  real figure:
  - Motion or opposition to modify a final order in an NRS 125, 125B, or 125C case: **$25** ("Domestic Case
    Reopening"). After a **joint-petition** decree, the first motion is **$154** and the first opposition **$82**.
  - Divorce complaint or joint petition $284; divorce answer or first appearance $202.
  - Civil complaint $255; civil answer or first appearance $208; summary-judgment motion $200.
  - Most follow-up filings carry no fee (User Guide). No per-transaction e-filing fee "at this time" (FAQ).
- **How paid:** credit card in eFlex, MasterCard or VISA, through Heartland, ideally from a card "token" saved in the
  lawyer's profile (User Guide, 2016). **Unconfirmed** after the 2026 system change: the accepted cards and the payment
  page. A fee-bearing paper is filed only after payment and clerk approval, but its timeliness is fixed at submission
  (NEFCR 9(a)(1), 9(d)(2)).

## 4. Drive the browser, attorney in the loop

Use the lawyer's real Chrome through Claude in Chrome, in a new tab.

1. Open `https://wceflex.washoecourts.com`. On a sign-in page, hand the tab to the lawyer and wait. Never type a
   username, password, code, or payment detail; never solve a CAPTCHA.
2. **eFile > Existing Case** (or Cases). Pick the case from My Cases, or enter the case number if the lawyer is not yet
   on it. Confirm the yellow existing-case banner and the caption.
3. On **Add a Document**: optional Document Category, then the **Document Type** matching the code on page 1;
   **Additional Text** in ALL CAPS if it helps the clerk; Browse to the file; **Add**. Repeat once per document, each
   exhibit separately. Copy each PDF into the session scratchpad first and check its sha256 against the Navigator copy.
4. Check the case's **Service List** (My Cases) to see who is served electronically and who needs paper (§5).
5. Open **Review and Approve**, screenshot it, and show the lawyer: case, department, each document type and file,
   page counts, and the fee.
6. **Stop there.** The lawyer clicks **Submit the Filing**, and enters or confirms any card personally. The agent never
   clicks Submit, Confirm, Pay, Add Credit Card, or a waiver option, and never opens an account, profile, or payment
   page. **Move to Draft** is allowed to stop midway. Avoid the browser Back arrow; sessions time out after about 20
   idle minutes (User Guide).

## 5. Service

E-filing **does serve** registered users: the Notice of Electronic Filing emailed by eFlex is effective service and has
the legal effect of paper service (NEFCR 10(b)); the electronic date is the legal service date, and paper sent too is a
courtesy ([FAQ](https://www.washoecourts.com/Efiling/FAQs)). A party not on eFlex is served by traditional means (NEFCR
10(d)), and the filer still owes proof of service (NEFCR 10(b)). A summons, a complaint or petition served with one, and
a subpoena are never served through eFlex (NEFCR 10(a)). **Unconfirmed:** whether opposing counsel from the decree is
still attorney of record for a post-decree motion; check the Service List and WDCR 23 before relying on it.

## 6. After Submit

- Status reads **Awaiting Approval**, then **Accepted** (User Guide). The stamp reads "FILED Electronically", the date
  and time, the clerk, and a **Transaction #** (NEFCR 9(c); seen on the court's own filed orders). **Unconfirmed:** the
  NEF and acceptance email sender and subject after the 2026 change; search Gmail for the case number.
- A nonconforming document can be cured within 7 days of the clerk's notice (NEFCR 9(b)(2)). Keep the deadline open.
- From the matter repository:

```bash
navigator mail save <gmail-url-of-the-acceptance-or-NEF>
navigator project efiling record "mail/<YYYY.MM.DD_subject>.eml" \
  --efsp eFlex --court "Second Judicial District Court" --case-number <case-number> \
  --filed-on <YYYY-MM-DD> --host <host>
```

- **Unconfirmed:** whether `efiling record` reads an eFlex email's transaction number. If it refuses, keep the `.eml`,
  say so, and do not retry with invented flags. File a hand-downloaded stamped copy with `navigator project efiling pair
  --transaction <transaction-number> <stamped.pdf> …`, or:

```bash
navigator site document upload --host <host> --project <code> --kind filing --folder "Docket Filings" \
  --slug "YYYY.MM.DD_<name>.pdf" --file <stamped.pdf>
```

- Complete or move the Deadline the filing satisfied with `navigator project deadline complete`.
- Archive the email threads (remove `INBOX`) only after the filing is recorded or the gap reported.

## 7. Timing

eFlex takes submissions around the clock; a document submitted by **11:59 p.m. Reno time** is filed that day, and the
submission time decides timeliness (NEFCR 9(d)-(e)). The Filing Office counter is open 8:00-5:00 weekdays ([Filing
Office](https://www.washoecourts.com/FilingOffice/FilingRules)); the Resource Center FAQ lists 8:00-4:30 Monday to
Thursday and 8:00-noon Friday. The court issues an administrative order for each eFlex outage that moves deadlines (AO
2026-11, 2026-19 to -21); read the list near a deadline. **Unconfirmed:** the court holiday list.

## 8. Read the docket and record deadlines

- **Case & Calendar Inquiry**, `https://caseinfo.washoecourts.com/` (free): Case Search, Calendar Search, Attorney
  Search. Old `washoecourts.com/Query/CaseInformation/<case>` links now redirect there. **Unconfirmed:** what the new
  portal shows for a family case, and whether it shows document images.
- **eFlex My Cases > History** shows the lawyer's own cases; a Subscription opens any public case's documents.
- Motion clock (WDCR 12): opposition 14 days after service; reply 7 days after the opposition; then either party files
  a Request for Submission (`3860`), and the motion is decided without argument unless the court orders a hearing.

Record every date as a Navigator Deadline, citing the order or rule:

```bash
navigator project deadline create --project <code> --host <host> --title "<what is due>" \
  --due-at <RFC 3339 with offset> --authority-kind court_order --authority "<order and date>" \
  --calendar-role firm --category court --source lawyer --ics notes/<YYYY.MM.DD>_<slug>.ics
```

Hearings take `--calendar-role hearing --category hearing`.

Source URLs too long to link:

```text
codes: https://www.washoecourts.com/Forms/GJ/15.%20Court%20Information%20and%20Requests/DocumentCodesList%20rev%208_21_26.pdf
```
