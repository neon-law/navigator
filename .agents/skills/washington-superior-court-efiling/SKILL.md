---
name: washington-superior-court-efiling
description: >
  Which e-filing system each Washington State superior court uses, whether attorneys must use it, how to register and
  how long it takes, fees, and whether it e-serves; plus where the Firm records the accounts it holds. Washington has no
  single e-filing system: eFileWA (AOC/Tyler) covers about a dozen counties, TrueFiling another handful, and King,
  Pierce, Clark, Thurston and others run their own. Use when a matter lands in a Washington county with no filing skill
  yet, when deciding which e-filing account to register, or when asked "can we e-file in <county>". Per-court steps live
  in the court's own skill (`clark-county-wa-efiling`, `benton-county-wa-efiling`); records are
  `washington-court-records`.
---

# Washington superior-court e-filing: which system, county by county

Surveyed 2026-11-11 from county Clerk pages. GR 30 sets statewide e-filing rules but leaves each county to choose a
system; the AOC keeps no central list. Re-check a row on the county's page before relying on it, and update it here.

## Which accounts the Firm holds

The Firm's accounts are Firm facts, kept in the Firm's own companion skill, never here. Read it before choosing a route.
Each account belongs to the signing lawyer and takes time to open:

| System | Registration | Covers |
| --- | --- | --- |
| Clark Serv-U | Form emailed to the Clerk; 4-6 weeks plus test filings | Clark only |
| TrueFiling | Self-registration, usable quickly | Benton, Chelan, Douglas, Klickitat, Spokane (Skamania unconfirmed) |
| eFileWA | Free; register through eFileWA after Tyler's training | 12 confirmed counties, list below |
| King KC Script, Pierce LINX | Register when a case lands there (both mandatory; LINX 2-3 days) | One county each |

Until an account is active, file on paper by mail or at the Clerk's counter.

## The three multi-county routes

- **eFileWA** (AOC, run by Tyler, `efilewa.tylertech.cloud`). Confirmed: Adams, Clallam, Franklin, Grays Harbor
  (mandatory from 2026-09-01), Kitsap, Lewis, Okanogan, Pacific, San Juan, Snohomish (mandatory, SCLGR 30), Whitman,
  Yakima. Possibly Columbia, Mason, Stevens (mandatory from 2027-01-01, unconfirmed), Wahkiakum, Whatcom. No platform
  fee; about 2.89% card fee. The AOC says it can e-serve. One account should cover every county (unconfirmed).
- **TrueFiling** (ImageSoft, `tf3.truefiling.com`). Benton, Chelan, Douglas, Klickitat, Spokane. Usually $5 per bundle
  (Spokane none). Optional in each. Does not serve.
- **Single-county systems:** King (Clerk's portal, mandatory, mandatory e-service), Pierce (LINX, mandatory,
  mandatory e-service), Clark (Serv-U, optional, no service), Thurston (`clerkefile.co.thurston.wa.us`), Grant
  (Laserfiche), Lincoln (web form), Pend Oreille (Box upload), Skagit (Granicus eSubmission), Jefferson (LiquidFiles),
  Island (email).

**Benton and Franklin share one superior-court bench but have separate Clerks and systems**: Benton is TrueFiling,
Franklin is eFileWA. Check the venue county on the caption.

## County table

| County | System | Attorneys must? | Register | E-serves? |
| --- | --- | --- | --- | --- |
| Adams | eFileWA | not stated | eFileWA | not stated |
| Asotin | none (joint local rule) | — | — | — |
| Benton | TrueFiling | no | TrueFiling + webinar | no |
| Chelan | TrueFiling | no | TrueFiling | not stated |
| Clallam | eFileWA | no | eFileWA | not stated |
| Clark | Serv-U | no | form, 4-6 weeks | no |
| Columbia | unconfirmed | | | |
| Cowlitz | none found: paper | — | — | — |
| Douglas | TrueFiling | not stated | TrueFiling | not stated |
| Ferry | none found | — | — | — |
| Franklin | eFileWA | not stated | eFileWA | not stated |
| Garfield | none (joint local rule) | — | — | — |
| Grant | Laserfiche portal | not stated | self-register; fees by phone | not stated |
| Grays Harbor | eFileWA | yes, from 2026-09-01 | eFileWA | not stated |
| Island | email PDF | no | none | no |
| Jefferson | LiquidFiles upload | no (LCR 10.7) | email Clerk | not stated |
| King | Clerk's eFiling (KC Script) | yes (LGR 30(b)(4)(A)) | portal | yes, mandatory |
| Kitsap | eFileWA | unconfirmed | eFileWA | not stated |
| Kittitas | county registration | no | county site | unconfirmed |
| Klickitat | TrueFiling | likely no | TrueFiling | not stated |
| Lewis | eFileWA | not stated | eFileWA | not stated |
| Lincoln | Clerk web form | no | ~24 h approval | no |
| Mason | unconfirmed | | | |
| Okanogan | eFileWA | not stated | eFileWA | not stated |
| Pacific | eFileWA | not stated | eFileWA | not stated |
| Pend Oreille | Box upload | not stated | free Box account | no |
| Pierce | LINX | yes (PCLGR 30(b)(5)) | form, 2-3 business days | yes, mandatory |
| San Juan | eFileWA | "must" per Clerk | eFileWA | not stated |
| Skagit | Granicus eSubmission | not stated | county login | no |
| Skamania | TrueFiling (unconfirmed) | | | |
| Snohomish | eFileWA | yes (SCLGR 30) | after webinar | yes |
| Spokane | TrueFiling | no, for now | TrueFiling | per-case e-service registration |
| Stevens | eFileWA (unconfirmed) | from 2027-01-01 (unconfirmed) | | |
| Thurston | Clerk's portal | unconfirmed | portal | unconfirmed |
| Wahkiakum, Walla Walla | unconfirmed | | | |
| Whatcom | e-filing since 2024-04-22, civil/probate; vendor unconfirmed | | | |
| Whitman | eFileWA | not stated | eFileWA | not stated |
| Yakima | eFileWA | not stated | eFileWA + Tyler training | not stated |

Almost every system excludes bench copies, protection orders, often ex parte papers, and original wills.

## Rules that do not vary

- No statewide attorney mandate exists or is pending. Order 25700-A-1658 (comment closed 2026-04-30) only makes GR 30
  electronic signatures permanent.
- Where the system does not serve, serve under CR 5, or by email where the party consented in writing (GR 30).
- District courts are separate systems; this skill is superior court only.
- The agent never registers, signs in, or clicks Submit; it drafts the registration and fills fields for the lawyer.

## Writing a new county skill

When a matter lands in a county, write `<county>-county-wa-efiling` from that county's Clerk page, using the Clark and
Benton skills as the shape, and record any new account in the Firm's companion skill.
