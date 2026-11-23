---
name: delaware-bankruptcy-ecf
description: >
  Files into a Chapter 11 case in the United States Bankruptcy Court for the District of Delaware through CM/ECF, as
  counsel to an individual creditor or claimant: notices of appearance and requests for service (Fed. R. Bankr. P. 2002
  and 9010), responses to omnibus claim objections under Del. Bankr. L.R. 3007-1, declarations, and certificates of
  service. Also covers the separate claims-agent process (proofs of claim go to the claims agent, not CM/ECF), reading
  the docket and omnibus agendas, and recording response deadlines and hearings in Navigator. Use whenever a Delaware
  bankruptcy paper is ready to file, a claim objection or CM/ECF notice arrives in a Delaware case, or the lawyer asks
  "where do I file this" for a Delaware Chapter 11.
---

# File into a Delaware Chapter 11 case as a creditor's counsel

Sources, checked 2026-11-11: the [Local Rules effective 2025-02-01][lr] (amendments proposed for 2027-02-01; check the
[rules page][lrp]), the Clerk's Administrative Procedures for Electronically Filed Cases (rev. October 2017), [NextGen
Information](https://www.deb.uscourts.gov/nextgen-information), [Claims Information][ci], [CM/ECF Attorney
Training][tr], and the FRBP at [Cornell LII][frbp] (2024 restyle). A judge's chambers procedures can override; read the
assigned judge's on the [court site][deb]. If a screen or rule differs, follow the court and update this skill.

## 1. Settle where it goes

- **Court:** U.S. Bankruptcy Court, District of Delaware, 824 N. Market St., Wilmington (Administrative Procedures).
- **Case number:** `YY-NNNNN (XXX)`. The caption carries the debtor, case number, assigned judge's initials, and
  docket number; a paper answering another carries that paper's title and docket number in its own title (L.R.
  9004-1(a)). In jointly administered cases, file in the lead case.
- **System:** CM/ECF on NextGen since February 2021. Each filer needs an individual PACER account linked to a Delaware
  filing account; shared accounts are barred; assistants file as filing agents under the attorney's account ([NextGen
  page](https://www.deb.uscourts.gov/nextgen-information)). Help desk: the address in Administrative Procedures.
- **Account:** the signing lawyer's own. The login is the lawyer's signature for every purpose (Administrative
  Procedures §3). The agent never signs in.
- **Delaware counsel:** pro hac vice needs Local Form 105 and association with Delaware counsel, who must be the
  CM/ECF filer and attend hearings (L.R. 9010-1(b)-(c)); otherwise Delaware counsel is due within 28 days of the first
  paper (L.R. 9010-1(d)). **Exception, L.R. 9010-1(e)(iii):** a party, pro se or through out-of-state counsel, may file
  or prosecute a proof of claim or a response to its claim; the Court may direct consultation with Delaware counsel if
  the claim litigation needs extensive discovery or trial time.

**Unconfirmed:** whether the Clerk issues a CM/ECF filing account to out-of-state counsel relying only on
9010-1(e)(iii), and whether that exception covers a bare notice of appearance. Ask the help desk before relying on it;
until answered, plan on Delaware counsel filing, or a paper filing the Clerk accepts.

## 2. What does not go through CM/ECF

- **Proofs of claim in a claims-agent case.** In a large Chapter 11 with a claims agent, the agent holds the original
  of every proof of claim and keeps the official claims register (L.R. 2002-1(e)(iii)-(iv)). The court's own electronic
  claim form takes claims only where no agent is assigned ([ci]). File through the agent's process named in the bar-date
  order and notice, and keep the agent's confirmation and claim number. Which agent serves which case: the court's
  [Claims Agency Case Assignments][cac]. A claim filed with an agent is withdrawn manually ([ci]). A claim transfer uses
  Local Form 138 with the claim number (L.R. 3001-1(b)).
- **Chambers deliveries.** Binders, courtesy copies, and proposed-order uploads follow the assigned judge's chambers
  procedures (one judge wants a binder of the proofs of claim with a claim objection; another takes a hyperlinked
  index). On request, a courtesy copy is marked "ECF CASE - COURTESY COPY" with the NEF attached (Administrative
  Procedures §2B).
- **The agenda** is the debtor's to file (L.R. 9029-3); a creditor files nothing for it.

## 3. Check the document before filing

- Final: no brackets, placeholders, or draft marks. Converted to PDF electronically, not scanned, where practicable
  (L.R. 5005-4(d)); each PDF at most 15 MB, with exhibits, proposed order, and certificate of service as attachments
  under one docket number (Administrative Procedures §1B). **PDF/A: unconfirmed** — neither source requires it.
- Signed `/s/ <Name>`; the filer keeps the wet-signed original of any declaration until one year after the case
  closes or appeals end (Administrative Procedures §3, §8; L.R. 5005-4(b)). Personal identifiers redacted (L.R. 9037-1).
- Font and spacing: L.R. 7007-2(a)(ii) (double-spaced, Times New Roman or Courier New, 12-point) applies by its
  terms to adversary-proceeding motions only. **Unconfirmed** for main-case papers; follow it anyway.
- **Notice of appearance and request for service:** name the party and counsel, with mailing and street address,
  phone, and email (L.R. 2002-1(d)); the attorney's name, office address, and phone (FRBP 9010(b)); and a request that
  all notices go to counsel (FRBP 2002(g), (i)). Service of chapter 11 motions then reaches every 2002 requester (L.R.
  2002-1(b)).
- **Response to an omnibus objection:** the deadline is in the objection's notice: at least 21 days after filing
  and 7 days before the hearing (L.R. 3007-1(g)); the objection itself must be filed and served 30 days before the
  hearing (FRBP 3007(a)(1)). File on or before the deadline (L.R. 9006-1(c)(ii)). Title it with the objection's title
  and docket number (L.R. 9004-1(a)); put the claim number and the exhibit that lists it in the text. Check whether the
  objection is substantive or nonsubstantive (L.R. 3007-1(c)); a nonsubstantive "no basis in books and records"
  objection needs the debtor's affidavit (3007-1(c)(vi)). Once the claimant responds, the debtor may amend only with
  written consent or leave (3007-1(e)(vi)). Whether L.R. 9013-1(h)'s bold hearing date and Article III consent statement
  apply to claim responses is **unconfirmed**; include both.
- A supporting declaration rides as an attachment or its own entry, signed `/s/`, with the original kept.

## 4. Drive CM/ECF, attorney in the loop

Use the lawyer's Chrome through Claude in Chrome, in a new tab.

1. Open the court's CM/ECF from [deb]. On the PACER sign-in, hand the tab to the lawyer and wait. Never type a
   username, password, one-time code, or card number; never solve a CAPTCHA.
2. **Bankruptcy** menu → choose the event. The court's [training][tr] names "Notice of Appearance" and "Request for
   Service of Notices" (Notices menu; `NTCAPR` is Notice of Appearance). The exact event for a response to a claim
   objection is **unconfirmed**: search the event list for "Response" and pick the one that links to a claim objection;
   ask the lawyer before choosing a near match. The filer picks the docket text from the court's events (Administrative
   Procedures §7).
3. Enter the case number, confirm the caption and judge, pick the party filer (the creditor; add it if absent).
4. Upload the main PDF from the session scratchpad (check its sha256 against the Navigator copy) and each attachment.
5. **Link to the entry being answered.** When asked "Should the document be linked to an existing event?", check it
   and select the omnibus objection's docket number. A linked response shows on the docket beside the objection.
6. Read the final docket-text screen, screenshot it, and show the lawyer: case, event, linked entry, docket text,
   files and page counts.
7. **Stop there.** The lawyer clicks **Next/Submit** personally. The agent never clicks Submit, Pay Now, or anything in
   Utilities, account, or payment pages. Notices and responses carry no fee; any fee is paid by card through Pay.gov
   (Administrative Procedures §4).

## 5. Service

- CM/ECF sends an NEF to every registered user, which is service on them; the filer's confirmation lists who got it
  (L.R. 9036-1(b); L.R. 5005-4(c)(ii); Administrative Procedures §2C).
- **Chapter 11 courtesy copy, required:** after CM/ECF service, also send a copy by email, mail, or hand to debtor's
  counsel, the U.S. Trustee's counsel, committee counsel, and every party whose rights the filing affects (L.R.
  9036-1(b)). The 2002 service list is on the claims agent's site or from the agent within 48 hours (L.R. 2002-1(c),
  (e)(vii)).
- Paper service is required for papers under FRBP 7004, a motion starting a contested matter, or a subpoena
  (L.R. 5005-4(c)(iii)). Anyone not on CM/ECF and not consenting to email is served by mail (+3 days, FRBP 9006(f)).
- File a certificate of service naming the document, the date, each party served with address or email, and the
  method; date it the day service actually happens.

## 6. After filing

- The NEF arrives from a `deb.uscourts.gov` reply address (observed; the court does not publish it).
  Save it from the matter repository with `navigator mail save <gmail-url>`.
- File the documents it points at (the NEF's free look is limited; run it promptly):

```bash
navigator project docket ingest mail/<YYYY.MM.DD_subject>.eml --dry-run --host <host>   # read the plan, then:
navigator project docket ingest mail/<YYYY.MM.DD_subject>.eml --host <host>
```

- `navigator project docket list` shows stored entries; `docket link-document <id> --slug …` repairs a missed pairing.
- Complete the Deadline the filing satisfied with `navigator project deadline complete <id> --project <code>`.
- Archive the NEF thread (remove `INBOX`) only after the documents are on file. Never trash.

## 7. Timing

- CM/ECF is open around the clock; for electronic filing the last day ends at **midnight in the court's time zone**,
  Eastern, unless a statute, rule, or order sets another time (FRBP 9006(a)(4)(A)). The response deadline in an
  objection notice usually names a time (often 4:00 p.m. ET); that controls.
- Count days per FRBP 9006(a)(1): exclude the trigger day, count weekends, roll a weekend or holiday last day forward.
  If the Clerk's office is inaccessible on the last day, time extends to the next accessible day (9006(a)(3)).
- If CM/ECF fails, email the help desk at once; a Request to Change Filing Date/Time can follow (Administrative
  Procedures §1C-D).

## 8. Read the docket and record deadlines

- **PACER**: $0.10 a page, $3 cap per document, waived at $30 or less a quarter; a party gets one free copy via the
  NEF ([PACER pricing](https://pacer.uscourts.gov/pacer-pricing-how-fees-work)).
- **Claims agent's case website**: claims register and full proofs of claim free (L.R. 2002-1(e)(viii)); agents
  usually mirror the docket free too (**unconfirmed** as a rule — check the site).
- **Omnibus hearings** are set by order; the debtor files the agenda by 12:00 p.m. ET two business days before
  (L.R. 2002-1(a)(i), 9029-3(a)(i)). Read each agenda for the objection's status: resolved, continued, or going forward.
  A matter is continued only with the movant's and objectors' consent (9029-3(a)(ii)); debtors often file a notice
  adjourning a claimant's objection to a later omnibus date. Move the Navigator hearing when that happens.
- A pro se claimant may appear remotely (L.R. 3007-1(f)); counsel follows the judge's appearance rules.

Record the response deadline and the hearing as Navigator Deadlines, citing the notice:

```bash
navigator project deadline create --project <code> --host <host> --title "Response to <Nth> omnibus objection" \
  --due-at <RFC 3339 with offset> --authority-kind court_rule --authority "Del. Bankr. L.R. 3007-1(g); D.I. <n>" \
  --calendar-role firm --category court --source lawyer --ics notes/<YYYY.MM.DD>_<slug>.ics
```

Use `--authority-kind court_order` when an order or adjournment notice sets the date. Hearings take `--calendar-role
hearing --category hearing`.

[lr]: <https://www.deb.uscourts.gov/sites/deb/files/Local%5FRules%5F2025%20(Clean).pdf>
[lrp]: <https://www.deb.uscourts.gov/local-rules-and-orders>
[ci]: <https://www.deb.uscourts.gov/claims-information>
[cac]: <https://media.deb.uscourts.gov/moveit/ClaimsAgentCases.html>
[tr]: <https://www.deb.uscourts.gov/cmecf-attorney-training>
[frbp]: <https://www.law.cornell.edu/rules/frbp>
[deb]: <https://www.deb.uscourts.gov/>

Source URLs too long to link:

```text
ap: https://deb.uscourts.gov/sites/deb/files/documents/administrative%20procedures%20for%20electronically%20filed%20cases.pdf
```
