---
name: washington-court-records
description: >
  Find and read Washington State court records: the statewide Washington Courts person search (dw.courts.wa.gov) to list
  every case a person is in, the statewide Odyssey Portal to read a superior-court case summary, hearings, and document
  list, how to read a Washington case number, and recording what is found in the matter and as Navigator Deadlines. Both
  sites sit behind a CAPTCHA the lawyer solves. Use whenever a Washington matter needs a case number, a party's other
  cases, a docket, a hearing date, or a case schedule. Filing is a different job, per court: `clark-county-wa-efiling`,
  `benton-county-wa-efiling`.
---

# Search Washington court records

Searching is statewide and the same in every county. Filing is per court and lives in that court's skill. Checked
2026-11-11 against the live sites; if a screen differs, follow it and update this skill.

**Hard rules.** The agent never solves a CAPTCHA, never signs in, and never registers an account. It fills the search
fields, then hands the tab to the lawyer to tick "I'm not a robot" and click Search or Submit. A record search is
read-only; nothing here files or serves anything. Results name real people: they go into the matter's repository only,
never into an issue tracker, a commit message, or another matter.

## 1. Which site answers which question

- **What cases is this person in, anywhere in Washington?**: **Person Search**, `https://dw.courts.wa.gov` (Washington
  Courts, Administrative Office of the Courts)
- **What is happening in this superior-court case: parties, events, hearings, documents filed?**: **Odyssey Portal**,
  `https://odysseyportal.courts.wa.gov/odyportal`
- **What does a filed document say?**: The county Clerk: a paid image subscription, a public terminal, or a copy
  request. See the county's filing skill.
- **What is set on a given day?**: Odyssey Portal **Search Hearings**, or the county's posted dockets

District and municipal courts (courts of limited jurisdiction) show **no docket online**: the person search lists the
case and its date only. Call or write that court for anything more.

## 2. Person search

1. Open `https://dw.courts.wa.gov/index.cfm?fa=home.namesearch&terms=accept&flashform=0` in a new tab.
2. Fill **First Name** and **Last Name** (both required); **Middle Name** narrows it. Find the fields with the
   `find` tool rather than fixed coordinates: the layout moves with the window size.
3. Hand off: the lawyer ticks the reCAPTCHA and clicks **Search**.
4. Read the results with `get_page_text`. Each row gives the **court**, the **name and role** (Petitioner, Respondent,
   Plaintiff, Defendant), the **case number without dashes** (e.g. `253014751`), and a **Court Info** date.
5. A name that is a link opens the docket. On 2026-11-11 superior-court rows showed **no link** even where the help
   text promises one; when there is none, go to the Odyssey Portal with the case number.

Search every spelling the client uses (maiden names, hyphenated names, nicknames) and both parties. A common name
returns other people: confirm by middle name, court, and role before attributing a case to anyone.

## 3. Read a Washington superior-court case number

`YY-T-NNNNN-D`, written with dashes on papers and without them in search results:

- `YY`: Year filed
- `T`: Case type. Confirmed: `2` civil (protection-order petitions appear here), `3` domestic (dissolution, parentage,
  modification). The Clark County Clerk's page also lists `4` and `5` as e-fileable new-case types; their names are
  **unconfirmed**.
- `NNNNN`: Sequence number
- `D`: One trailing digit. It varies between cases in the same county (`…-1`, `…-3`, `…-9`), so it is a **check digit,
  not a county code**. Always copy it from the court; never compute it or substitute a county code.

Write `25-3-01475-1` from the search result `253014751`: two digits, one, five, one.

## 4. Odyssey Portal

1. Open `https://odysseyportal.courts.wa.gov/odyportal` → **Smart Search**.
2. Type the case number with dashes (or a name as `Last, First Middle`). **Advanced Filtering Options** narrows by
   court and case type.
3. Hand off: the lawyer ticks the reCAPTCHA and clicks **Submit**.
4. Open the case: read the case summary, parties and attorneys of record, events, hearings, and the list of documents
   filed. Look for a **case schedule** or scheduling order and every hearing.
5. Document images depend on the county. Some counties show none in the portal (Benton does not use Odyssey document
   management); others sell an image subscription. The county's filing skill says which.
6. **Register / Sign In** unlocks more for some counties, by a per-person registration with that county's Clerk. The
   agent never registers or signs in.

## 5. Record what was found

In the matter repository's own skill (`.agents/skills/<code>-matter/SKILL.md`), add a **Court cases** table: case
number, court, the client's role, the Court Info date, and what the case is, with the search date as its source. Mark
any case not yet read as unknown and ask the client about it. Correct any case number recorded earlier from memory or an
email.

Record every date from the docket as a Navigator Deadline:

```bash
navigator project deadline create --project <code> --host <host> --title "<what is due>" \
  --due-at <RFC 3339 with offset> --authority-kind court_order --authority "<order and date>" \
  --calendar-role firm --category court --source lawyer --ics notes/<YYYY.MM.DD>_<slug>.ics
```

Hearings take `--calendar-role hearing --category hearing`. A date the lawyer sets internally (pull the docket, appear)
is `--authority-kind internal --category internal`.

**Unconfirmed:** the names of case types other than `2` and `3`; whether the person search ever links a superior-court
docket now; what the Odyssey Portal shows after sign-in, county by county.
