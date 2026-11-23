---
name: los-angeles-superior-efiling
description: >
  Files into an existing unlimited civil case in the Superior Court of California, County of Los Angeles (case numbers
  like `25STCVNNNNN`): motions, oppositions, replies, notices, discovery motions, declarations, proposed orders, and
  proofs of service, through a court-approved EFSP. Covers what may not be e-filed, the General Order's PDF and bookmark
  rules, fees, the attorney-in-the-loop browser run that stops at the EFSP's summary, electronic service and its proof,
  the midnight rule, printed courtesy copies, reserving hearing dates in the Court Reservation System, and filing the
  stamped copy back into Navigator. Use whenever a Los Angeles Superior Court civil document is ready to file, a
  deadline depends on that court's docket, or the lawyer asks "where do I file this" for a Los Angeles matter.
---

# File into a Los Angeles Superior Court unlimited civil case

Sources, checked 2026-11-11: the court's [efiling page][efiling]; the [First Amended General Order re Mandatory
Electronic Filing for
Civil](https://lascpubstorage.blob.core.windows.net/cpw/LIBOPSCivil-36-1stAmendedGeneralOrderMandatoryEfilingCivil.pdf)
(2019-GEN-014-00, 2019-05-03; "General Order"); the [Civil Efiling FAQ][faq] (rev. 2024-05-24; "FAQ"); [Local Rules ch.
3](https://www.lacourt.ca.gov/courtrules/currentcourtrulespdf/chap3.pdf) (eff. 2022-01-01); the [2026 Civil Fee
Schedule](https://lascpubstorage.blob.core.windows.net/cpw/LIBSVCExecutiveSupport-414-2026FeeSchedule050626.pdf) (eff.
2026-05-06); California Rules of Court [2.251][r2251], [2.257][r2257] and [3.1110][r31110]. The court is moving from
`lacourt.org` to `lacourt.ca.gov`; old links redirect or 404. If a screen or rule differs, follow the court and update
this skill.

## 1. Settle where it goes

- **Court**: Superior Court of California, County of Los Angeles, Civil Division. `CV` in `25STCVNNNNN` marks civil;
  what the year and `ST` positions encode is unconfirmed (check the case summary, §8).
- **Mandatory?**: **Yes.** Represented parties must e-file in unlimited civil since 2019-01-02 (General Order preamble
  and ¶2(b); CRC 2.253(b); Local Rule 3.4(a); FAQ 3). Exemption is by court order on forms EFS-007/EFS-008 (FAQ 3).
- **Route**: An **approved EFSP**. The court accepts no direct e-filing and no fax filing (FAQ 7-10). Any listed EFSP
  may be used and changed at any time (FAQ 11, 13).
- **EFSP list**: [Approved EFSPs - Civil](https://www.lacourt.ca.gov/apps/efiling-providers/ecourts) (renders only in a
  browser). Pick one that offers electronic service (§5) and card or e-check payment (FAQ 16, 57).
- **Account**: The signing lawyer's own EFSP account. Which EFSP the Firm uses is a Firm fact: read it from the Firm's
  own skill, or ask the lawyer. The agent never signs in or handles the password.
- **Complex cases**: A separate general order and document-name list apply (FAQ 72). Not covered here.

## 2. What is not e-filed

General Order ¶4; FAQ 6, 27. These go to the Civil Clerk's Office of the assigned courthouse, 8:30-4:30, or by mail:

- Peremptory challenges (CCP 170.6) and challenges for cause (CCP 170.3).
- Bonds and undertakings.
- Trial and evidentiary-hearing exhibits.
- An ex parte application filed with a new complaint.
- Documents lodged conditionally under seal. The motion to seal *is* e-filed, and a conformed copy goes with the
  lodged documents to the courtroom (FAQ 51).
- Papers attached to a Notice of Lodgment are lodged in paper; the Notice of Lodgment itself is e-filed (¶4(b)).

## 3. Check the document before the envelope

- Final: no brackets, placeholders, or draft marks. Formatted to [CRC 2.100 et
  seq.](https://courts.ca.gov/cms/rules/index.cfm?title=two) (General Order ¶2(a)); FAQ 53 lists non-compliance as a
  rejection ground.
- First page, below the case number: hearing date, time, department, and judge, if known (CRC 3.1110(b)). Pages
  numbered consecutively (3.1110(c)).
- **Text-searchable PDF**, not password-protected (General Order ¶6(a); FAQ 64). Scans at 300 DPI (FAQ 62).
- **Bookmarks** for the table of contents and for every declaration, exhibit, deposition or transcript excerpt, proof
  of service, and points and authorities, each linked to its first page and titled to identify it (General Order
  ¶6(b)-(d); CRC 3.1110(f)(4)). Unbookmarked filings are rejected (FAQ 53).
- **One PDF per document.** Motion, declaration, proposed order, and proof of service are separate PDFs in one envelope
  (¶6(f)-(g); FAQ 41). A proposed order on pleading paper also needs a `.doc`/`.docx` copy (FAQ 42, 45).
- Redact under CRC 1.201: minors' initials, last four of an SSN, birth year only (¶6(j)).
- Signatures (General Order ¶11; CRC 2.257). A document not under penalty of perjury is deemed signed by the e-filer
  (2.257(c)). A declaration needs the declarant's qualifying e-signature or a wet-signed original the filer keeps and
  produces within five days of a demand (2.257(b)). CRC 2.256 is the filer's responsibilities rule, not signatures.
- **Fees** (2026 schedule): motion or other paper requiring a hearing, including a discovery motion or noticed ex parte,
  $60 (GC 70617(a)); summary judgment or adjudication $500; continuance of a hearing $20; stipulation and order $20;
  amended notice of motion and case management statement no fee. A party's **first paper** (answer or other first
  appearance, unlimited) is $435 (GC 70612). EFSP charges are extra and vary (FAQ 14). Fees are assessed by the **Filing
  Document Name**, so pick the exact one from the [Civil Document Name
  List](https://www.lacourt.org/division/efiling/pdf/CivilDocumentNameList.pdf), or a generic "Motion (name extension)"
  with the title typed in (FAQ 25-26).

## 4. Drive the EFSP, attorney in the loop

Use the lawyer's Chrome through Claude in Chrome, in a new tab. Follow the matter repository's `browser-handoff` skill
if it has one.

1. Open the Firm's EFSP. On a sign-in page, hand the tab to the lawyer and wait. Never type a password, a code, or
   payment details, and never solve a CAPTCHA.
2. Start a filing into an **existing case**: court Los Angeles, case type unlimited civil, the **courthouse location**
   the case is assigned to (a wrong location is rejected, FAQ 35), then the case number. Confirm the caption.
3. Add each PDF with its Filing Document Name. Mark the motion as **Lead Document** (FAQ 28). Copy each PDF into the
   session scratchpad first and check its sha256 against the Navigator copy.
4. Filing party and attorney must match the document's caption and signature block (FAQ 53). Payment statement:
   standard fees unless a fee waiver or GC 6103 applies.
5. Service: choose the EFSP's file-and-serve option only if §5 says electronic service is right, and tick the service
   contacts from the last proof of service in the matter. Do not add or remove a contact without asking.
6. Go to the review or summary screen. Screenshot it and show the lawyer: case number, location, each document name and
   page count, lead document, service list, fees.
7. **Stop there.** The lawyer clicks **Submit** and any **Pay** personally. An envelope cannot be cancelled once
   submitted (FAQ 55). The agent never touches account, payment, or contact settings. Saving a draft is allowed.

## 5. Service

- **E-filing serves no one.** The court does not serve for a party (FAQ 56); serve through the EFSP or directly
  (CRC 2.251(f)(1); FAQ 57).
- **Consent.** Because e-filing is mandatory, a party that e-files has agreed to accept electronic service at its
  electronic service address (FAQ 59; CRC 2.251(c)(3)). A self-represented or exempt party must consent affirmatively
  (CRC 2.251(b)); otherwise serve by mail or hand.
- Electronic service is complete as provided in [CCP
  1010.6](https://leginfo.legislature.ca.gov/faces/codes_displaySection.xhtml?lawCode=CCP&sectionNum=1010.6) (Cal. Rules
  of Court, rule 2.251(i)). The two-court-day extension for e-service and its current subdivision are **unconfirmed**
  (leginfo sat behind a challenge page on 2026-11-11); read 1010.6 before counting a response date.
- **Proof of service**: e-service is proved under CCP 1013b (CRC 2.251(j)(1)), and the proof must be e-filed (FAQ 46).
  Proof of e-service of moving papers is due at least five court days before the hearing (CRC 2.251(j)(2)).

## 6. After filing

The court aims to accept or reject within two business days; a document is filed only when the court stamps it (FAQ 18
and 21). A rejection notice states the reason: correct and resubmit, and keep the deadline open (FAQ 23). The stamp sits
in the upper right of page 1; a proposed order comes back with a "Received" ribbon, and a signed order follows through
the EFSP or by mail (FAQ 32, 43-44). From the matter repository:

```bash
navigator mail save <gmail-url-of-the-EFSP-acceptance-email>
navigator project efiling record "mail/<YYYY.MM.DD_subject>.eml" \
  --efsp <efsp-name> --court "Superior Court of California, County of Los Angeles" \
  --case-number 25STCVNNNNN --filed-on <YYYY-MM-DD> --host <host>
navigator site document upload --host <host> --project <code> --kind filing --folder "Docket Filings" \
  --slug "YYYY.MM.DD_<name>.pdf" --file <stamped.pdf>
```

Whether `efiling record` can parse this EFSP's acceptance email is unconfirmed; if it refuses, keep the `.eml`, say so,
and do not invent flags. Then complete or move the Deadline the filing satisfied with `navigator project deadline
complete`. File the EFSP receipt as the `clark-county-nv-efiling` skill does. Archive the EFSP emails (remove `INBOX`)
only after the stamped copy is filed; never archive a rejection until it is resubmitted.

## 7. Timing

- **Midnight rule.** A document received before midnight on a court day is filed that day if accepted; one received on
  a non-court day is filed the next court day (Local Rule 3.4(c); General Order ¶7(a)(i); FAQ 20; both cite CCP
  1010.6(b)(3) and CRC 2.253(b)(6)). The current statute text was not read directly; confirm the subdivision on leginfo.
  E-filing does not change any "last day to file" (FAQ 22).
- **Ex parte**: application e-filed by 10:00 a.m. the court day before the hearing; opposition by 8:30 a.m. on the
  hearing day, with a printed courtesy copy that day (General Order ¶8; FAQ 48). The applicant must appear (FAQ 49).
- A failed transmission not the filer's fault can be fixed by motion to deem it timely (General Order ¶7(a)(ii)).

## 8. Read the docket, courtesy copies, and deadlines

- **Case summary**: [Civil Case Access][access] takes the case number. Document images and name searches cost a fee
  (charged on download) and need an account or guest sign-in; hand the tab to the lawyer for any sign-in or payment.
- **Hearing dates**: motions are not scheduled by e-filing. Reserve a date in the **Court Reservation System**,
  [portal-lasc.journaltech.com/public-portal](https://portal-lasc.journaltech.com/public-portal/) (FAQ 37), or call the
  department if it does not use CRS. **E-file the motion within three business days** of reserving or the reservation is
  cancelled and the motion rejected (FAQ 38). Rescheduling is allowed until 10 days before the hearing, for $20 (FAQ
  39-40). Some departments require an Informal Discovery Conference before a motion to compel; the reservation is a
  lawyer action, not the agent's.
- **Printed courtesy copies**, with proof of electronic submission, are required regardless of timing for: pleadings and
  motions of 26 pages or more, anything with points and authorities, demurrers, anti-SLAPP filings, summary judgment
  motions, motions to compel further discovery, and anything a standing order requires (General Order ¶9(b); FAQ 60). A
  filing within two days of the hearing needs a courtesy copy in the courtroom by 4:30 p.m. that day, or 10:00 a.m. the
  next business day if e-filed after 4:30 (¶9(a)). Read the department's [Courtroom
  Information](https://www.lacourt.org/courtroominformation/ui/search.aspx?ons=xcivil) page too.

Record every hearing, CRS filing deadline, and opposition or reply date as a Navigator Deadline, citing its source:

```bash
navigator project deadline create --project <code> --host <host> --title "<what is due>" \
  --due-at <RFC 3339 with offset> --authority-kind court_order --authority "<order, notice, or rule>" \
  --calendar-role firm --category court --source lawyer --ics notes/<YYYY.MM.DD>_<slug>.ics
```

Hearings take `--calendar-role hearing --category hearing`.

[efiling]: <https://www.lacourt.org/division/efiling/efiling2.aspx>
[faq]: <https://lascpubstorage.blob.core.windows.net/cpw/LIBOPSCivil-33-CivilEfilingFAQ.pdf>
[r2251]: <https://courts.ca.gov/cms/rules/index.cfm?title=two&linkid=rule2%5F251>
[r2257]: <https://courts.ca.gov/cms/rules/index.cfm?title=two&linkid=rule2%5F257>
[r31110]: <https://courts.ca.gov/cms/rules/index.cfm?title=three&linkid=rule3%5F1110>
[access]: <https://www.lacourt.ca.gov/pages/lp/access-a-case/tp/find-case-information/cp/os-civil-case-access>
