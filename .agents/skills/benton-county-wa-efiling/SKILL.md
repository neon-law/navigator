---
name: benton-county-wa-efiling
description: >
  File into, and read the docket of, a case in Benton County Superior Court, Washington (Benton & Franklin Counties
  Superior Court, sitting at the Justice Center in Kennewick, Clerk's offices in Kennewick and Prosser): dissolutions
  with children, responses, motions, declarations, and ex parte restraining orders. Covers the Clerk's optional
  TrueFiling e-file portal, what must go on paper, the 3:00 p.m. review cutoff, the $5 bundle fee, the daily 1:30-3:30
  ex parte docket, eMotion bench copies and proposed orders, service (which e-filing does not do), the Odyssey Portal
  docket read, and getting the stamped copy back into Navigator. Use whenever a Benton County, Washington superior-court
  document is ready to file or present ex parte, a deadline depends on that court's docket, or the lawyer asks "where do
  I file this" for a Tri-Cities (Kennewick, Richland, Prosser) matter.
---

# File into a Benton County, Washington superior-court case

Sources, checked 2026-11-11: the Clerk's [E-Filing][efile] page and Document Filing Policy (2024-02-01), [Odyssey
Portal][odp], [Civil Protection Orders][cpo], [News & Alerts][news], [Fee Schedule][fees], and Clerk [home][clerk]; the
Superior Court's Ex Parte page (effective 2026-09-01) and weekly dockets; and the 2025 Benton/Franklin Local Rules (LGR,
LDR). If a screen or rule differs, follow the court and update this.

## 1. Settle where it goes

- **Court**: Benton & Franklin Counties Superior Court, Benton County Justice Center, 7122 W. Okanogan Place, Bldg. A,
  Kennewick (LGR 1(D)). Domestic cases are case type `3` (LGR 4(A)(3)), e.g. `YY-3-NNNNN-D`. The last digit is a check
  digit, not a county code (`washington-court-records` §3).
- **Clerk**: Kennewick, 7122 W. Okanogan Pl., Ste. A210, 8:00-4:00; Prosser, 620 Market St., 8:00-4:00, closed 12-1;
  telephone and email on the [Clerk][clerk] page.
- **Portal**: **TrueFiling**, `https://tf3.truefiling.com/` ([E-Filing][efile]). Not eFileWA and not Odyssey File &
  Serve: eFileWA is Benton County *District* Court's system, mandatory there, and not this court. **Franklin County's
  Clerk does use eFileWA for superior court**: a case venued in Franklin goes through eFileWA, not TrueFiling
  (`washington-superior-court-efiling`). The Clerk's case system is Tyler **Enterprise Justice** (formerly Odyssey).
  TrueFiling support contacts are on the [E-Filing][efile] page.
- **Mandatory?**: **No.** TrueFiling is optional and carries fees; mail, courier, and the counter still work
  ([E-Filing][efile]). An attorney drop box sits by the Clerk's office, 2nd floor ([News][news]).
- **Account**: The signing lawyer's own TrueFiling account. The local rules waive GR 30(d)'s court-issued user ID (LGR
  9(A)). How an account is opened is **unconfirmed** (the page links only a training webinar); check TrueFiling's
  sign-up and the Clerk. The agent never signs in or handles the password.

**Cases from 2018 on are loaded in TrueFiling.** For an older case, email the Clerk and allow 24 hours.

## 2. What cannot be e-filed

Paper only, by mail, counter, or drop box ([E-Filing][efile]): anything in a **sealed case**; writs of garnishment,
restitution, execution, and habeas corpus; orders of sale; Joel's Law petitions; Blake-decision documents; foreign
subpoenas; **registration of an out-of-state custody order**; trial exhibits; and any color or grayscale document, which
TrueFiling would convert to black and white (mail the whole document instead).

Not through TrueFiling, by its own route:

- **Civil protection orders** (DV and the rest): in person (preferred) or the Clerk's free JotForm **E-Submission**,
  state forms only, PDF, bitonal, one document per file ([CPO][cpo]). E-Submission is for CPO papers only.
- **Bench copies, Read Firsts, and domestic proposed orders**: upload to **eMotion**, never the Clerk. Domestic proposed
  orders are mandatory and "should not be filed with the Clerk" (LGR 2, LDR 4(B)).
- **Agreed or default final orders**: paper to Court Administration with the Final Order Submission cover sheet by
  3:00 p.m. five court days before the hearing; never eMotion or email (LDR 5).

## 3. Check the document before the envelope

- Final: no brackets, placeholders, or draft marks. Formatted to **GR 14**; declarations in at least 12-point type.
- Signed `s/ <Name>`, WSBA No., the Firm's name, address, phone, email; GR 30(a)(4) e-signatures are accepted
  (LGR 9(B)).
- **300 dpi, bitonal (black and white)**, one document type per file; mixed bundles are rejected ([E-Filing][efile]).
- A **new case** needs a completed Case Information Cover Sheet or it is rejected ([E-Filing][efile]).
- Ex parte papers carry the attestation on whether any judicial officer is recused or disqualified (LGR 6(C)).
- **Confidential Information form (FL All Family 001), page 1, item 4, has no optional fields for the filer.** The Clerk
  rejected a whole new-case bundle (2026-11-13) because page 1 was "incomplete, missing some of the plaintiff's
  information". Before the client signs, every petitioner field on page 1 must hold an answer: item 3 (address
  confidential: No/Yes) checked, interpreter No/Yes checked, full name, date of birth, sex, driver's licence or ID
  number and state (or "none"), race, mailing address, home address box checked (or the address written in, or "no fixed
  address"), email, phone, SSN, and employer name, phone, and address (or "unemployed"). A blank is a rejection; write
  "none" or "unknown" rather than leave one. The other party's fields may be explained on the signature page's
  "unavailable because" line.
- **One rejected document rejects the whole bundle.** The Clerk sends one notice per document and asks you to "resubmit
  the entire bundle". Pre-flight every document, not only the one you changed.
- **GR 34 waives the filing fee only.** The **$5 bundle fee** and the **$30 ex parte fee** "may not be waived" (Clerk,
  2026-11-13). At TrueFiling checkout, do not mark the whole envelope indigent: attach the GR 34 motion, waive only the
  filing fee, and pay $35 by card. Decide before the day who pays the $35 (client, or the Firm advances it).
- **Read the PDF's text layer, not just the picture, before the client signs.** A .docx form rendered to PDF wrapped
  words in narrow cells ("Bento / n", "U / ntil", a case number split across two lines), and the client then signed the
  damaged text. Run `pdftotext -layout` on every page and look for split words, then read every rendered page.
- **Fees**: a **$5 bundle fee per case number**, not waivable; ex parte **$30**, not waivable; a rejected filing is
  refunded except the card processing fee, so payment is by card in TrueFiling ([E-Filing][efile]). TrueFiling's own
  vendor fee is **unconfirmed**. The linked schedule (effective 2025-07-27) lists dissolution $364 and modification of a
  Benton decree $56 ([Fees][fees]), but the page announces a new schedule effective 2026-06-11 that it does not link:
  **confirm the figure with the Clerk** before filing a fee-bearing paper.

## 4. Drive the browser, attorney in the loop

Use the lawyer's Chrome through Claude in Chrome, in a new tab.

1. Open `https://tf3.truefiling.com/`. On the sign-in page, hand the tab to the lawyer and wait. Never type a username,
   password, code, or card detail, and never solve a CAPTCHA.
2. Find the case by number and confirm the caption. If it is not there and predates 2018, stop: email the Clerk.
3. Copy the PDF into the session scratchpad, check its sha256 against the Navigator copy, attach it, and pick the
   document type. Rename the Navigator `YYYY.MM.DD_name.pdf` copy for upload only if the form asks.
4. Go to the review or summary screen, screenshot it, and show the lawyer: case, document type and title, page count,
   fees.
5. **Stop there.** The lawyer clicks **Submit** (and any **Pay**) personally. The agent never clicks Submit, Pay, or an
   upload that files, nor anything on an account, payment, or contact settings page. Screen labels here are
   **unconfirmed** (no public walkthrough); follow the screen and update this skill.

## 5. Service is separate

Assume TrueFiling **serves no one**: the Clerk's page offers no service, and whether TrueFiling has an e-service feature
here is **unconfirmed**. Serve under CR 5; e-service on a party only with that party's consent (CR 5(b)(7)); otherwise
mail (three days added) or hand delivery, then file proof. The court and Clerk email attorneys at their WSBA directory
address (LGR 5). The petitioner serves the Automatic Temporary Restraining Order on each respondent, and the Domestic
Case Scheduling Order within ten days of receiving it (LDR 1(D)-(E)). Bench copies go to eMotion only after the paper
has been served (LGR 2(A)).

## 6. After Submit

- TrueFiling emails a submission confirmation; **that is not a filing**. A second email says accepted (filed) or
  rejected ([E-Filing][efile]). A rejection is not a filing: fix, resubmit, keep the deadline open.
- Review is fast: a 14:13 submission was rejected at 14:51 the same day. On a rejection, save every notice
  (`navigator mail save <id> --name YYYY.MM.DD_truefiling-rejected-NN.eml`; they share a subject, so the default name
  collides), log the reason verbatim as a Communication with `--role court`, and add the lesson to this skill.
- From the matter repository, save each email with `navigator mail save <gmail-url>`, then file the stamped copy:

```bash
navigator site document upload --host <host> --project <code> --kind filing --folder "Docket Filings" \
  --slug "YYYY.MM.DD_<name>.pdf" --file <stamped.pdf>
```

- Whether the acceptance email carries a stamped copy is **unconfirmed**. For ex parte papers it does not: order
  conformed copies through `www.bentonclerkpr.com` ([E-Filing][efile]).
- Complete or move the Deadline the filing satisfied with `navigator project deadline complete`.
- Archive the threads (remove `INBOX`) only after the stamped copy is filed.

## 7. Timing

- **A paper is filed when the Clerk's stamp is applied**, however it arrived (Policy), not on submission.
- TrueFiling submissions after **3:00 p.m.** are not reviewed until the next judicial day; allow 24-48 hours to appear
  in Enterprise Justice, and 48 hours for ex parte filings. For a deadline, or a **noon cutoff** for a docket, the Clerk
  says file **in person** or by process server ([E-Filing][efile]). Drop-box papers after 4:00 p.m. are filed the next
  judicial day ([News][news]).
- On a deadline day, do not rely on TrueFiling: the lawyer files at the counter or by a messenger.

### Ex parte presentation

- **Benton: daily, 1:30-3:30 p.m.; the courtroom opens at 1:15.** The room is on the Daily Schedule. **No WebEx.**
  Papers go in the Ex Parte box by 3:30; the Clerk calls the matter. Bring conforming copies to leave with signed
  copies; otherwise copies are sold from 1:00 p.m. the next judicial day (Ex Parte, [News][news]).
- A protection order after 3:30: complete paperwork to the Clerk by **4:00 p.m.**, or it is returned
  (Ex Parte).
- Ex parte is procedural, not a substitute for a docket. **Change of custody only on a clear showing of present
  danger**, through the court's Motions for Immediate Orders process (Ex Parte).
- **Notice first**, or good-faith efforts, unless an exception applies: immediate restraining orders, orders setting a
  show-cause hearing only, fee waivers, or a good-faith belief notice will cause physical or irreparable harm to the
  party or a child (LGR 6(A)). Agreed or stipulated orders may be presented ex parte (LGR 6(B)).
- Motions to shorten time follow the policy on the court website (LGR 3(C)); its text is **unconfirmed**.

## 8. Read the docket and the case schedule

Finding the case and reading its docket is statewide: follow `washington-court-records`. What is particular to Benton
County:

- **Odyssey Portal**, `https://odysseyportal.courts.wa.gov/odyportal`: case summary, events, and hearings. Benton
  registration is a Portal Master Registration Form plus a confidentiality agreement **per person**, mailed to the
  Kennewick Clerk. It shows **no document images**; Benton does not use Odyssey document management ([Portal][odp]).
- Documents: the two public terminals on the Justice Center's 2nd floor (case number needed), or copies via
  `www.bentonclerkpr.com`, $0.50 a page, $0.25 emailed ([News][news], [Fees][fees]).
- Domestic dockets: Tuesday 8:15 a.m. Over Ten (A-L) and 1:15 p.m. (M-Z); Wednesday 8:30 Under Ten, 9:00 Over 20;
  Thursday CPO (weekly). Courtrooms change: check the Daily Schedule.
- Motion clock (LGR 3, LDR 4): motion **21** calendar days before the hearing, response **11**, reply
  **5**; confirm with a Read First on eMotion by **3:00 p.m. five calendar days** before, or the motion is stricken.
- The Clerk issues the Domestic Case Scheduling Order on filing; mandatory appearances are in bold; financial
  disclosures are due by its deadline. A parenting seminar certificate is filed before any final parenting plan (LDR
  1(E)-(F)).

Record every date from the case schedule as a Navigator Deadline, citing the order or rule:

```bash
navigator project deadline create --project <code> --host <host> --title "<what is due>" \
  --due-at <RFC 3339 with offset> --authority-kind court_order --authority "<order and date>" \
  --calendar-role firm --category court --source lawyer --ics notes/<YYYY.MM.DD>_<slug>.ics
```

Hearings take `--calendar-role hearing --category hearing`.

[efile]: <https://www.bentoncountywa.gov/government/elected%5Fofficials/clerk/e%5Ffiling.php>
[odp]: <https://www.bentoncountywa.gov/government/elected%5Fofficials/clerk/odyssey%5Fportal.php>
[cpo]: <https://www.bentoncountywa.gov/government/elected%5Fofficials/clerk/civil%5Fprotection%5Forders/index.php>
[news]: <https://www.bentoncountywa.gov/government/elected%5Fofficials/clerk/important%5Fnews%5Falerts.php>
[fees]: <https://www.bentoncountywa.gov/government/elected%5Fofficials/clerk/fee%5Fschedule.php>
[clerk]: <https://www.bentoncountywa.gov/government/elected%5Fofficials/clerk/index.php>

Source URLs too long to link:

```text
policy: https://www.bentoncountywa.gov/Documents/Government/Elected%20Officials/Clerk/E%20Filing/DocumentFilingPolicy197042817020224PM.pdf
exparte: https://www.bentoncountywa.gov/departments/courts_and_judicial_services/superior_court/schedules_and_calendars/ex_parte.php
weekly: https://www.bentoncountywa.gov/departments/courts_and_judicial_services/superior_court/schedules_and_calendars/benton_franklin_dockets_weekly_schedule.php
lcr: https://www.bentoncountywa.gov/Documents/Departments/Courts%20and%20Judicial%20Services/Superior%20Court/Local%20Rules%20and%20Required%20Forms/LCR-2025BentonFranklinCountiesEmergencyLCReffective1120251391073232123125PM.pdf
```
