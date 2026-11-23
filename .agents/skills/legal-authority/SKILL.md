---
name: legal-authority
description: >
  Quote, cite, link, and store U.S. legal authority — cases, statutes, regulations. Use whenever a response, memo,
  portal, notation, or commit quotes or cites law, or when asked to research a legal question, save a case, or build an
  authority list. Covers verify-before-citing, the mandatory link rule, where full text is allowed to live (Navigator
  Assets, never git), and the Authority model that replaces "save it as a case".
---

# Legal authority: quoting, linking, and storing it

Three rules, in priority order. The first is about honesty, the second about re-verification, the third about privilege.

1. **Never cite what you have not opened.** A search hit is a lead, not a citation.
2. **Every citation carries a link**, except inside a Portal bundle, where links are
   forbidden by a build gate. See *Where links go*.
3. **Full text never enters git.** It goes to Navigator Assets. See *Where bytes go*.

## Verify before citing

A search result gives you a title and a highlight. That is not enough to quote from, and the highlight is frequently not
the language you want.

| Source | Find | Then open — required before citing |
| --- | --- | --- |
| Statute, regulation, constitution | `searchLaws` | `analyzeLaw` (by `id` or `citation`) |
| Case law | `search` | `analyzeOpinion`, or `findInOpinion` for a quote |
| Concept / issue search, treatment | descrybe `search_cases_by_concept` | `get_case_summary`, `get_case_passages` |
| Docket, filings | CourtListener `search` | `get_endpoint_item` |

Two traps worth naming:

- **A subsection pinpoint is not a lookup key.** `18 U.S.C. § 924(c)(1)(C)` and popular
  names like "ERISA § 514" go through `searchLaws` first; passing them to `analyzeLaw` as a citation either misses or
  silently truncates to the section.
- **Check treatment before relying on a case.** descrybe returns a treatment rollup;
  midpage returns a citator status. A case with `Caution` or `distinguished` needs that said out loud in the write-up,
  not omitted because it weakens the point.

## Where links go

**In chat, memos, research write-ups, commit messages, and Authority records: always link.** Use the most specific URL
the tool gave you.

- midpage `analyzeLaw` returns a `deeplinkUrl` per passage — use it for a
  passage-specific proposition or a quote. Use the document-level `url` only for a general reference. Link each
  subsection you actually rely on, and never invent a path segment.
- midpage `search` results carry a document `url`.
- descrybe results carry a `url` to the case viewer.
- Format as a Markdown link on the citation itself, not as a bare URL beside it.

**In a Portal bundle: never.** The Portal's build gate scans the built output and fails on any host that is not our own,
because an off-origin request leaks a referrer, an IP, and a timing signal about a privileged matter from the client's
own browser. So a portal carries the citation as text — which is what a citation is for — and the link lives in the
Authority record the portal references.

## Where bytes go

**"No legal files, ever." Git never stores them.** No PDFs, no Westlaw copies, no downloaded slip opinions, no exhibits.
`.gitignore` blocks the common extensions so the rule survives a careless `git add`.

This is not a gap in the design — Navigator already has the home:

- An **Authority** carries "its citation, its title and publisher, its canonical URL,
  and an archived artifact so it survives link rot." The archived artifact *is* the saved full text, and it is a
  first-class field.
- The bytes land in the **`assets`** table: content-addressed at `blobs/<sha>`, deduped
  by `sha256_hex`, in object storage. `visibility` (`internal` by default, or `client`) gates whether it ever reaches a
  Portal.

So "download the case and save it" is right as an instinct and wrong as a destination. Saving it into a Project
repository makes global reference data project-scoped, which is backwards — see below.

## The noun is Authority, not a case or a pleading

Look the word up before using it: `navigator glossary show "Authority"`.

> One case, statute, regulation, administrative proceeding, or secondary source, as
> **global reference data** … It is deliberately **not case-shaped**: a statute is a
> first-class Authority, not a case record with its fields bent to fit.

Three consequences that are easy to get wrong:

- **There is no `case` or `pleading` notation kind, and adding one would be a mistake.**
  `kind:` is closed; `navigator validate` rejects anything outside it. The list today is `retainer`, `letter`, `filing`,
  `will`, `trust`, `directive`, `agreement`, `onboarding`, `memo`, `event`, `post`, `workshop`, `github`, and the
  workbench kinds — among them **`authority_library`**, which is the notation shape for a *collection* of authorities. A
  notation is an executable agreement: it has a respondent, a questionnaire, and a signature. A published opinion has
  none of those, so notation is the wrong container for one case however the `kind:` question is answered.
- **An Authority carries no `project_id`.** The same authorities recur across matters. A
  matter's *use* of one is a separate scoped record holding which side relies on it (`ours`, `adverse`, `neutral`) and
  what the firm did with it.
- **Never embed citation prose in a composition.** The glossary names this as the failure
  mode: "it drifts from the record and cannot be re-verified." A composition references an Authority by id and the
  server resolves it under the lens gate.

### Pleading, order, and opinion are three different things

If a task or a request says cases are orders and orders are pleadings, that chain is wrong at both links, and it is
worth correcting rather than building on.

[Fed. R. Civ. P. 7(a)](https://app.midpage.ai/laws/uscode/t28a/Civil/tIII/rule7-58a3a1) is a closed list: *"Only these
pleadings are allowed"* — complaint, answer to a complaint, answer to a counterclaim designated as a counterclaim,
answer to a crossclaim, third-party complaint, answer to a third-party complaint, and, if the court orders one, a reply
to an answer. Rule 7(b) puts motions somewhere else entirely: *"A request for a court order must be made by motion."*

- A **pleading** is filed by a party and frames the issues. Closed list of seven.
- A **motion** is a request for an order. Not a pleading; Rule 7(b) keeps it separate.
- An **order** is issued by the court. Not on the Rule 7(a) list, and not a party filing.
- An **opinion** is the court's reasoning. It commonly accompanies an order but is not the
  operative directive, and a reported case is ordinarily the opinion.

So a case is not an order, and an order is not a pleading. Use **Authority**.

## Privilege: what may never reach a client view

A matter's disposition on an authority is a closed taxonomy, and several of its values record **firm reasoning** —
`reviewed-not-used`, `record-exhibit-not-relied-on`, `captured-exhibit-not-quoted`, `monitoring-not-relied-on`. A client
who reads "reviewed, not used" learns the firm's strategic assessment of their own matter, which discloses work product
rather than data. **None of these may ever enter a client-facing allowlist.**

When writing a client-facing surface, state the law and what it means for the client. Do not state what the firm
considered and rejected.

## The write path

`navigator site authorities` files or corrects a global Authority and its archived artifact; read its `--help` for the
verbs. If a verb you need is missing, report it as a Navigator dependency. Do not route around it with a bespoke
endpoint, an ad hoc HTTP request, a second datastore, or a file committed to a Project repository as a stand-in.

## Checklist

Before saying a citation is done:

- [ ] Opened with `analyzeLaw` / `analyzeOpinion` / `get_case_passages`, not just searched
- [ ] Quote is verbatim — not reflowed, repunctuated, or tidied
- [ ] Treatment checked, and any caution stated in the write-up
- [ ] Linked, with the passage-level URL where the point is passage-level
- [ ] No link inside a portal bundle
- [ ] No PDF, DOCX, or slip opinion added to git
- [ ] Called an Authority, not a case record or a pleading
- [ ] Nothing recording firm reasoning on a client-facing surface
