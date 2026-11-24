---
name: harris-county-tx-efiling
description: >
  Files into an existing civil case in a Harris County, Texas district court (civil district courts such as the 125th;
  cause numbers like `2025-NNNNN`) through eFileTexas: answers, motions, responses, notices of appearance and
  designations of counsel, and certificates of service. Covers whether the signing lawyer may appear at all (pro hac
  vice), what may not be e-filed, the format and redaction rules the clerk enforces, the attorney-in-the-loop browser
  run that stops at the EFSP's summary, mandatory e-service, the 10:00 a.m. Monday answer deadline and the midnight
  rule, reading the District Clerk's docket, and filing the stamped copy back into Navigator. Use whenever a Harris
  County district-court civil document is ready to file, a deadline depends on that court's docket, or the lawyer asks
  "where do I file this" for a Houston matter.
---

# File into an existing Harris County, Texas district-court civil case

Sources, checked 2026-11-11: [TRCP](https://www.txcourts.gov/media/1463426/texas-rules-of-civil-procedure.pdf)
(txcourts.gov PDF, rules last amended 2026-10-01 per the [rules
page](https://www.txcourts.gov/rules-forms/rules-standards/)); the District Clerk's [e-filing FAQ][FAQ], [Online
Services](https://www.hcdistrictclerk.com/e-services/eservices.aspx), and [civil and family fee schedule][FEES] (updated
2026-07-15); the JCIT [Technology Standards
v7.1](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf) (effective 2023-10-11; the
clerk's link is named "7.0" but serves 7.1); the [EFSP list](https://www.efiletexas.gov/service-providers.htm); the
Board of Law Examiners' [non-resident attorney page][PHV]; the Civil Trial Division [local rules][LR] (version dated
2014-04-28); the clerk's [attorney access sheet][ATTY] (2018). If a screen or rule differs, follow the court and update
this skill.

## 1. Settle where it goes

- **Court**: the numbered civil district court on the citation (e.g. 125th District Court, Civil Courthouse, 201
  Caroline, Houston; [Justex][JUSTEX]). Cause numbers read `YYYY-NNNNN`; copy the exact form from the citation.
- **Clerk**: Harris County **District** Clerk. The County Clerk is a separate office; each rejects the other's
  filings ([FAQ]).
- **Mandatory**: TRCP 21(f)(1), attorneys must e-file where mandated. The Supreme Court's order (Misc. Docket No.
  12-9206, amended by 13-9092 and 13-9164; Rule 21 comment) reached Harris County civil courts 2014-01-01; paper only
  from pro se filers, no fax ([FAQ]).
- **Portal**: eFileTexas, the OCA's filing manager, reached only through a certified EFSP (21(f)(3)). The EFSP is the
  lawyer's choice; eight are listed free, first the "State Provided EFSP", and one login works across EFSPs ([EFSP
  list](https://www.efiletexas.gov/service-providers.htm)).
- **Account**: the signing lawyer's own. The agent never signs in, never sees the password, never stores it.

**Can the signing lawyer appear?** A lawyer not licensed in Texas who lives outside Texas must be admitted pro hac vice
under Rule 19 of the Rules Governing Admission to the Bar of Texas and Gov't Code §82.0361: pay the $250 per-attorney,
per-cause fee to the Board of Law Examiners, get its acknowledgment letter (next business day), then file a motion with
the court attaching that letter and a motion from a resident Texas attorney; the court grants or denies ([PHV]). Until
it is granted, the Texas attorney signs and files. Unconfirmed: the rule's current text (read it on txcourts.gov) and
how the TRCP 57 bar-number line reads for an admitted non-Texas lawyer. Confirm both before the first filing; never file
under a non-Texas lawyer's signature alone.

## 2. What is not e-filed

- **Sealed or in-camera documents**, and anything access-restricted by law or order, must not be e-filed (TRCP
  21(f)(4)(B)). TRCP 76a sealed material goes to the District Clerk's office on paper ([FAQ]).
- Wills need not be e-filed (21(f)(4)(A)); a court may allow paper for good cause (21(f)(4)(C)).
- **Proposed orders are e-filed**, as the "Proposed Order" document type, which is not file-stamped
  ([TECH](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf) §4.8.1). Local Rule 3.3.1
  requires one with every motion and response, as a separate instrument ([LR]).
- No paper copy unless a local rule asks (21(f)(9)); a court's own procedures may, so check its page.

## 3. Check the document before the envelope

- Final: no brackets, placeholders, or draft marks. Dated the day it will be submitted, certificate included.
- **Text-searchable PDF**, converted directly rather than scanned where possible, not locked (TRCP 21(f)(8)); 8.5x11,
  properly rotated, no package or embedded PDFs, no embedded fonts, 300 DPI if scanned
  ([TECH](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf) §3.1).
- **One document per PDF.** Lead document, exhibits, and proposed order are separate files in one envelope, or the
  envelope is returned ([FAQ]; [TECH](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf)
  §4.8.4). Envelope limit 35 MB ([FAQ]).
- **File name**: Latin1 alphanumerics only, no special characters, 50 characters at most
  ([TECH](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf) §3.1.F). Rename the
  Navigator `YYYY.MM.DD_name.pdf` copy (drop `.` and `_` if the EFSP rejects them) for upload only.
- **Signature block** (TRCP 57; 21(f)(2), (7)): `/s/ <Name>`, State Bar of Texas number, the Firm's name, address,
  telephone, and **email** (required on every e-filed document), fax if any.
- **Redact sensitive data** (TRCP 21c): government ID, SSN, tax ID, financial account numbers, birth dates, home
  addresses, and names of anyone who was a minor when suit was filed. Use `X` per character or a visible redaction; keep
  the unredacted copy. If the data is required, flag the document as sensitive in the envelope (21c(d)).
- **Motions**: certificate of conference on every motion, plea, and special exception except those Local Rule 3.3.6
  excludes; label an unopposed motion "Unopposed" in the caption (3.3.5); a submission-docket motion names a **Monday at
  8:00 a.m.** at least 10 days after filing (3.3.3) ([LR]). The local rules seen date from 2014; check
  topics.txcourts.gov and the court's own procedures for a newer version.
- **Fees** ([FEES]): the schedule lists no fee for a plain answer, motion, notice, or response. A counterclaim or
  cross-action, third-party action, intervention, motion for new trial, or motion for contempt is $80. The EFSP may add
  its own charge. A wrong filing code mis-bills and can be returned ([FAQ]).

## 4. Drive the EFSP, attorney in the loop

Use the lawyer's real Chrome through Claude in Chrome, in a new tab. The field names below come from the Tyler envelope
the `clark-county-nv-efiling` skill drives; eFileTexas is the same vendor, but these labels are unconfirmed here. If a
screen differs, follow the screen and update this skill.

1. Open the lawyer's EFSP. On a sign-in, MFA, or CAPTCHA page, hand the tab to the lawyer and wait. Never type a
   password, code, or payment detail, and never solve a CAPTCHA.
2. **File into existing case** → **Location**: Harris County District Clerk (exact label unconfirmed; a "Harris County
   - County Clerk" location would be the wrong office) → search the cause number → confirm the caption and court.
3. **Filing Type** `EFileAndServe` (or the EFSP's "file and serve" option): filing without serving falsifies the
   certificate of service.
4. **Filing Code** from the statewide list
   ([TECH](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf) §4.1.2): `Answer/
   Response` for an answer or response; `Motion (No Fee)`, or the specific fee-bearing motion code; `Notice` for a
   notice of appearance or designation of counsel; `Proposed Order` for the order; `Counter Claim/Cross Action/...` when
   the answer carries one. The clerk publishes `FIELD_CODES.xlsx` (2024-08-15) on its public-datasets page; confirm
   against the live drop-down.
5. **Description**: the exact title on the document ([FAQ]). Upload each PDF from the session scratchpad after
   checking its sha256 against the Navigator copy. Mark sensitive data only if 21c(d) applies.
6. Service contacts: check every party's attorney on the e-service list; do not add or remove one without asking.
7. Payment: the lawyer's saved account only. Open the **Summary**, screenshot it, and show the lawyer the cause number,
   court, codes, descriptions, files and page counts, service contacts, and fees.
8. **Stop there.** The lawyer clicks **Submit** personally. The agent never clicks Submit, Pay, Resubmit, or Cancel,
   and never opens an account, payment, or contact settings page. Save as Draft is allowed.

## 5. Service

- **E-service is mandatory**: a document e-filed must be served through the EFM on every party or attorney whose email
  is on file with it; otherwise serve in person, by mail, commercial delivery, fax, or email (TRCP 21a(a)).
- Serve **each attorney in charge** where several parties have different counsel (21(c)). The attorney in charge is the
  first signer of a party's first pleading unless another is designated; a change of designation is written notice to
  the court and all parties served under 21a (TRCP 8). That is the designation-of-counsel filing.
- E-service is complete on transmission to the serving party's EFSP (21a(b)(3)); mail adds three days to the
  recipient's response period (21a(c)).
- **Certificate of service** on every filed paper, over signature (21(d), 21a(e)), naming the method and the date.
- A hearing or application notice must reach all parties at least three days before the setting (21(b)).

## 6. After Submit

The clerk must accept, return for correction, or reject on the business day received, or the next
([TECH](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf) §4.8.3). A **returned**
filing can be copied, corrected, and resubmitted keeping its original file date within 72 hours
([TECH](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf) §4.8.4); the [FAQ] gives
seven days for sensitive-data returns. A returned filing is not a filing: keep the deadline open. The accepted, stamped
copy stays downloadable for 30 days
([TECH](https://www.hcdistrictclerk.com/common/forms/pdf/Technology%20standards%5F7.0.pdf) §3.1.G); save it at once. The
sender address of eFileTexas notifications is unconfirmed; search Gmail for the envelope number.

```bash
navigator mail save <gmail-url-of-the-acceptance-email>
navigator project efiling record "mail/<YYYY.MM.DD_subject>.eml" \
  --efsp <efsp-name> --court "<nth> District Court, Harris County, Texas" --case-number <cause-number> \
  --filed-on <YYYY-MM-DD> --host <host>
navigator site document upload --host <host> --project <code> --kind filing --folder "Docket Filings" \
  --slug "YYYY.MM.DD_<name>.pdf" --file <stamped.pdf>
navigator project deadline complete ...   # the Deadline the filing satisfied; check --help for its flags
```

If `efiling record` refuses the email (it has refused Tyler envelope emails that name no transaction id), keep the saved
`.eml`, file the stamped PDF as above, say so, and do not retry with invented flags. Archive each email thread (remove
`INBOX`) only after it is saved and the stamped copy is filed.

## 7. Timing

- **Midnight rule**: a document is timely if e-filed before midnight Central on the deadline, **unless it must be filed
  by a certain time of day**. It is filed when transmitted to the EFSP, but one transmitted on a Saturday, Sunday, or
  legal holiday is filed the next day that is none of those (TRCP 21(f)(5)).
- **Counting** (TRCP 4): exclude the trigger day, include the last day; a last day on a Saturday, Sunday, or legal
  holiday runs to the end of the next day that is none of those. Weekends and holidays are skipped in periods of five
  days or less, except the three-day mail periods of Rules 21 and 21a.
- **Answer**: by **10:00 a.m. on the Monday next after 20 days** from service of citation (TRCP 99(b), (c)). It is a
  time-of-day deadline, so the midnight rule does not save it. Whether Rule 4 moves a Monday that is a legal holiday is
  the lawyer's call; file by the preceding business day instead.
- Initial disclosures generally fall due 30 days after the answer (TRCP 99(c) notice); record that date too.
- Outage: ask the court for relief; a rules deadline earns a reasonable extension (21(f)(6)). Holidays: unconfirmed.

## 8. Read the docket and record deadlines

- **District Clerk search** ([ESV](https://www.hcdistrictclerk.com/e-services/eservices.aspx)): Search Our Records and
  Documents, `/eDocs/Public/Search.aspx`, shows parties, documents, costs, and settings; Docket Search is
  `?dockettab=1`. Whether search now needs a login is unconfirmed.
- **Registration** (New User, `/eDocs/Public/NewUserAcknowledgement.aspx`): a lawyer who enters a **Texas** bar number
  gets docket search by bar number, document views, free uncertified copies, and docket and case email alerts ([ATTY],
  2018; confirm current). Otherwise documents cost $1.00 a page, all sales final
  ([ESV](https://www.hcdistrictclerk.com/e-services/eservices.aspx) new-user page). A non-Texas lawyer likely lacks the
  attorney features (unconfirmed). Registration is the lawyer's own; never register for them.
- Clerk contact for a filing problem: the e-filing address in the [FAQ], with cause number and envelope number ([FAQ]).

Record every date from the citation, a setting, or an order as a Navigator Deadline, citing the rule or order:

```bash
navigator project deadline create --project <code> --host <host> --title "<what is due>" \
  --due-at <RFC 3339 with offset> --authority-kind court_order --authority "<order and date, or TRCP rule>" \
  --calendar-role firm --category court --source lawyer --ics notes/<YYYY.MM.DD>_<slug>.ics
```

Hearings and submission dates take `--calendar-role hearing --category hearing`.

[FAQ]: <https://www.hcdistrictclerk.com/common/civil/EFileFAQ.aspx>
[FEES]: <https://www.hcdistrictclerk.com/Common/Civil/pdf/Fee%5FSchedule%5FCivil%5Fand%5FFamily.pdf>
[PHV]: <https://ble.texas.gov/non-resident-attorney-fee-info>
[LR]: <https://topics.txcourts.gov/LocalRulesPublic/PreviewAttachment/702>
[ATTY]: <https://www.hcdistrictclerk.com/InstructionalDocs/HCDCWebSite%5FNew%20Features%5FAttorney.pdf>
[JUSTEX]: <https://www.justex.net/location>
