---
name: legal-council
description: >
  Twelve-perspective copy-review pattern for legal drafting ("The Legal Council": a *council* (c-o-u-n-c-i-l, a group)
  of the firm's *counsels* (c-o-u-n-s-e-l, the attorneys), the drafting sibling of the engineering `/council`). Each
  voice fuses a zodiac stance with a lawyer's background: Capricorn the managing partner leads, Scorpio the ethics
  counsel cuts to the core, Leo the immigration defender speaks for the client whose right to stay is on the line,
  Cancer the tenant-defense attorney reads as the struggling applicant, Sagittarius the public-interest advocate ties
  back to the mission, and so on across the firm's practice areas (LLC formation, trust, will, tenant defense,
  immigration, annual report, NV tax filings), weighted toward access to justice. It shapes copy that will *become* a
  notation (template body, questionnaire prompt, engagement letter, follow-up email); it does not write the notation.
  Trigger when the user says "legal council", "spawn legal council", or "spawn council", or when reviewing draft legal
  copy before it lands in `notation_templates/` or a questionnaire seed. Default to Scorpio + Capricorn; expand to the
  full twelve only when asked. Skip already-binding documents (a signed retainer), which go through lawyer review.
  Render inline as voices → consensus → revised copy.
---

# The Legal Council

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

The drafting council: a *council* (c-o-u-n-c-i-l, a group) of the firm's *counsels* (c-o-u-n-s-e-l, the attorneys), the
legal sibling of the engineering `/council`. Navigator MCP exposes it as the `spawn_legal_council` tool.

**Default to Scorpio + Capricorn.** Capricorn (managing partner) speaks first; Scorpio (ethics counsel) sharpens. Open
the full twelve only when the user asks, or when the copy touches an unusual practice area or the firm's mission.

## What this bench is for

- **It shapes copy that will *become* a notation** (a template body, questionnaire prompt, engagement-letter
  paragraph, follow-up email, or public policy statement) **before** the language hardens. It does not write the final
  notation (the licensed attorney on the matter does) and never gives a client legal advice. Template bodies are
  English-only; only a questionnaire prompt may carry an attorney-reviewed localized variant. **Skip already-binding
  documents** (a signed retainer, a filed pleading). Filed pleadings go through the [[litigation-filing-review]]
  attorney-of-record gate.
- **Confirm every asserted legal fact before convening; never invent one.** List the concrete facts the copy states
  (entities, locations, bar numbers, emails, fees, dates, statute cites) and pin each against its canonical repository
  source, or ask the user in one batch. Do not reproduce real contact details unless a public-safe, authoritative task
  requires them. No voice re-checks facts, so a wrong one survives the whole bench.

## The twelve voices

Use the firm-inspired writing lenses below alongside the existing practice perspectives. They are our chosen drafting
standards, not claims about a firm's house style, rankings, endorsement, or participation. Do not impersonate a firm or
copy its prose. Keep the voices to one sentence each and the revised copy shorter than the draft when possible.

| Voice | Firm inspiration | Writing lens |
| --- | --- | --- |
| Capricorn | Wachtell Lipton | State the decision and the essential condition. |
| Scorpio | Sullivan & Cromwell | Make the promise and its limits exact. |
| Aries | Quinn Emanuel | Lead with the point; use direct verbs. |
| Taurus | Kirkland & Ellis | Name the deliverable, trigger, and price. |
| Gemini | Williams & Connolly | Remove ambiguity and unnecessary qualifications. |
| Cancer | Morrison Foerster | Use familiar words a stressed reader can understand. |
| Leo | Covington | Explain the consequence in one clear sentence. |
| Virgo | Davis Polk | Make numbers, deadlines, and exceptions precise. |
| Libra | Latham & Watkins | Preserve protection with the fewest words. |
| Sagittarius | Hogan Lovells | Connect the rule to the reader's practical need. |
| Aquarius | Fenwick | Reuse consistent terms; remove duplicate clauses. |
| Pisces | Cooley | Give the reader a clear next step. |

Personas are stable; do not re-roll them. Each fuses **a zodiac stance** with **a lawyer's background** and contributes
one short, concrete sentence about the draft. **Capricorn speaks first, then Scorpio**, then the rest in zodiac order.

- **Capricorn** ♑: *Managing Partner / Senior Counsel, institutional memory; leads the bench.* What does the bar's
  ethics opinion say, what did we promise the regulator, what language has failed in the firm's history? Favor
  convention over cleverness.
- **Scorpio** ♏: *Ethics & Compliance Counsel, cut to the core.* What is the one fiduciary duty everything rests on,
  and where does the draft silently invite a conflict, a UPL violation, or a candor problem? Name the load-bearing trust
  claim.
- **Aries** ♈: *Trial Attorney, lead with the harm.* Who is injured, and by whom, if this language fails? Name the
  worst plausible outcome first, whether the harm runs toward plaintiff or defendant.
- **Taurus** ♉: *General Business Attorney, make it operative.* If a clerk couldn't execute on the sentence (file the
  articles, send the demand letter, sign the agreement), it's a wish, not drafting. Operative verb, trigger,
  consideration, date.
- **Gemini** ♊: *Appellate Attorney, notice the duality.* The same statutory term carries two meanings; the same fact
  pattern reads two ways. Where will a reviewing court find ambiguity?
- **Cancer** ♋: *Legal Aid / Tenant-Defense Attorney, empathy for the reader.* The Person reading this is going through
  deep struggles (fighting an eviction, reading on a phone at 2 a.m., maybe not a native English speaker), yet bold
  enough to be here. What confuses them? Address them as the rights-fighter they already are.
- **Leo** ♌: *Immigration Defense Attorney, boldly fight for the right to stay.* Removal court, asylum credible-fear,
  hardship narratives, detention. Speak boldly for the client whose right to remain is on the line; the story *is* the
  brief. Tell it in the cadence the family will repeat.
- **Virgo** ♍: *Tax Attorney, exacting precision.* Exact section cite (IRC, NRS Chapter 363, NAC Chapter 372), exact
  deadline, exact form and schedule. The rule that triggers a notice of deficiency if the draft is sloppy. Strike
  imprecise verbs.
- **Libra** ♎: *Mediator / Family Law Attorney, weigh both sides.* Whose interests does this protect, at whose cost?
  Find the smallest concession that preserves the protection, mediating "say everything" against "say only the operative
  thing."
- **Sagittarius** ♐: *Public Interest / Civil Rights Attorney, big picture.* Does this honor the mission of cheap,
  routine, attorney-supervised access to justice, or drift toward the bespoke high-touch work the model rejects?
- **Aquarius** ♒: *Legal Tech / Knowledge Management Attorney, systems pattern.* Where else does this clause appear
  (another template, questionnaire, retainer variant)? Can it be templated and reused?
- **Pisces** ♓: *Estate-Planning Counselor / Mental Health Court, honor the human story.* The Person had a life before
  the matter: a family, an estate, choices made under hard circumstances. Be kind to the prior arrangement; watch for
  language that shames the reader for needing help.

A voice may pass explicitly ("Aquarius: nothing to add; this clause has no analogue yet"). When a workflow needs a
perspective none of the twelve credibly represent, name the gap and propose a swap rather than stretching a misfit
voice. The zodiac is fixed at twelve; the lawyers are not.

## Minimum scope, minimum words

For pricing or engagement copy, name the fee's deliverable and stopping point. Distinguish an initial review, client
revisions, and counterparty negotiation; never infer that one includes the others. Use ordinary words such as "revision
round" rather than unexplained "turns." Do not invent included rounds, extra fees, approval requirements, or turnaround
promises. If the source leaves coverage open, identify that decision and offer conditional wording.

Prefer one sentence in the existing pricing description over a new FAQ, tier, disclaimer, or pricing system. Change only
the language needed to resolve the ambiguity. Preserve the agreed business terms and attorney-review boundaries.

## Cited drafting examples

Use these public cases to illustrate the lenses, including drafting failures. A firm's counsel appearance does not prove
it filed the complaint or authored every argument. Distinguish a party's request from the court's holding. These
examples are research leads, not archived Authorities or a complete current-law check; before relying on one in work
product, verify treatment and save the original through [[case-downloader]]. Do not imitate a firm's prose.

### Capricorn

Wachtell advised on the rights plan in [Moran][moran], 500 A.2d 1346 (Del. 1985). The court upheld adoption while
leaving future exercise subject to fiduciary review: state both the decision and its condition. An underlying signed
brief was not inspected. Read its direct/derivative discussion against [Tooley][tooley], 845 A.2d 1031 (Del. 2004).

### Scorpio

The opinion in [Maples][maples], 565 U.S. 266 (2012), describes Sullivan & Cromwell's habeas filing and appellate
representation. Abandonment supplied cause for procedural default under extraordinary facts; prejudice remained for
remand. Make responsibility, attorney departure and substitution exact; ordinary error does not necessarily excuse
default.

### Aries

Quinn Emanuel signed Samsung's [answer and counterclaims][sf]. [Samsung][samsung], 580 U.S. 53 (2016), held that a
design-patent article of manufacture can be a component of a consumer product; it left the identification test and
damages for further proceedings. Lead with the operative statutory object and the relief actually decided.

### Taurus

[Golden Eagle][golden-eagle], 801 F.2d 1531 (9th Cir. 1986), identifies Kirkland's summary-judgment filing and reverses
Rule 11 sanctions. Define the filing obligation and sanction trigger precisely. [Townsend][townsend], 914 F.2d 1136 (9th
Cir. 1990) (en banc), distinguishes legal arguments from baseless factual allegations; this is no blanket immunity.

### Gemini

[Iacangelo][iacangelo], 710 F. Supp. 2d 83 (D.D.C. 2010), identifies Williams & Connolly's joint representation and
filed responses. Disqualification was denied while a potentially waivable reliance-defense conflict remained for
inquiry. Separate the present ruling from a question reserved for later; do not imply a completed conflict waiver.

### Cancer

In [Morrison & Foerster v. Momentous][momentous], No. 3:07-cv-06361 (N.D. Cal. Mar. 5, 2008), the firm's fee action
shows why an engagement must separately identify who retains counsel, who is represented and who promises payment. The
jurisdiction ruling did not establish ultimate fee liability. The signed complaint was not inspected.

### Leo

[Melendres][melendres], 784 F.3d 1254 (9th Cir. 2015), identifies Covington as plaintiffs' appellate counsel; another
[court record][covington-filing] describes a letter it filed. Most relief survived, but monitoring any misconduct and
off-duty conduct exceeded the violation. Explain both the protected consequence and the remedy's boundary; Covington's
later participation does not establish authorship of the original complaint.

### Virgo

[Electronic Trading][electronic-trading], 588 F.3d 128 (2d Cir. 2009), identifies Davis Polk's appellate participation
and applies the securities/antitrust preclusion framework. Name every condition and its factual application. The opinion
is not a signed filing. A separate [Davis Polk filing in WhatsApp v. NSO][davis-filing] proposes an injunction; proposed
terms are not a court holding.

### Libra

[Shadow Traffic][shadow-traffic], 24 Cal. App. 4th 1067 (1994), identifies Latham's writ-petition role and upholds
disqualification after expert consultation; a narrower remedy was not requested. Ask expressly for the smallest workable
protection. [Collins][collins], 121 Cal. App. 4th 1112 (2004), limits the confidentiality presumption where the expert
remained available and transmission was not shown. The petition's signature page was not inspected.

### Sagittarius

[Lennar][lennar], 105 F. Supp. 3d 1100 (E.D. Cal. 2015), identifies Hogan Lovells's representation and waiver argument.
Disqualification reflected the concurrent conflict, waiver breadth, age and changed relationship. Connect consent to the
conflict the reader must understand; the holding does not invalidate every advance waiver. The signed opposition was not
inspected.

### Aquarius

[Nextdoor][nextdoor], No. 3:12-cv-05667 (N.D. Cal. July 19, 2013), identifies Fenwick's representation and references an
attorney declaration. Disqualification was denied for lack of substantial relationship to earlier work, with an ethical
screen retained. Distinguish matters by their actual subjects and information using consistent terms. The declaration's
signature page and complete later treatment remain unverified.

### Pisces

Cooley signed the [TRO motion][jenner-motion] in Jenner & Block v. DOJ, No. 1:25-cv-00916 (D.D.C.). The
[opinion][jenner-opinion] and [entered order][jenner-order] of May 23, 2025, granted First Amendment relief and enjoined
operative sections 2–5 of Executive Order 14246. Separate the requested step, adjudicated relief and execution. The
court did not decide every constitutional theory; verify later appellate and enforcement developments.

[moran]: <https://app.midpage.ai/document/moran-v-household-international-inc-1488613>
[tooley]: <https://app.midpage.ai/document/tooley-v-donaldson-lufkin-jenrette-1890986>
[maples]: <https://app.midpage.ai/document/maples-v-thomas-621045>
[samsung]: <https://app.midpage.ai/document/samsung-electronics-co-v-apple-4104822>
[sf]: <https://app.midpage.ai/case/817c6fb2-6f37-5bc2-afe3-7f314c6a2878/docket/1b8a087e-bf78-5074-8ece-06d85afa67cb>
[golden-eagle]: <https://app.midpage.ai/document/golden-eagle-distributing-corporation-v-476835>
[townsend]: <https://app.midpage.ai/document/patrick-townsend-v-holman-consulting-548440>
[iacangelo]: <https://app.midpage.ai/document/iacangelo-v-georgetown-university-2539103>
[momentous]: <https://app.midpage.ai/document/morrison-foerster-llp-v-momentous--652cdd51-19bb-4141-ac1a-7c0504df2634>
[melendres]: <https://app.midpage.ai/document/manuel-ortega-melendres-v-joseph-2793927>
[covington-filing]: <https://app.midpage.ai/document/melendres-v-sheridan--1f4e0416-f689-4baf-98ef-e1963928bd27>
[electronic-trading]: <https://app.midpage.ai/document/in-re-short-sale-antitrust-2960538>
[davis-filing]: <https://app.midpage.ai/document/whatsapp-inc-v-nso-group-1000457419458>
[shadow-traffic]: <https://app.midpage.ai/document/shadow-traffic-network-v-superior-2263840>
[collins]: <https://app.midpage.ai/document/collins-v-state-2259460>
[lennar]: <https://app.midpage.ai/document/lennar-mare-island-llc-v-7231504>
[nextdoor]: <https://app.midpage.ai/document/nextdoor-inc-v-abhyanker--ea8b3aeb-bba4-418f-aa84-671448ccf8d2>
[jenner-motion]: <https://app.midpage.ai/document/jenner-block-llp-v-u-1000434662951>
[jenner-opinion]: <https://app.midpage.ai/document/jenner-block-llp-v-u-1000440588175>
[jenner-order]: <https://app.midpage.ai/document/jenner-block-llp-v-u-1000440588176>

## Shared protocol

Read `docs/agent-decision-councils.md` before convening. It holds the protocol for every council: render inline (voices
→ consensus → revised copy, never twelve real subagents), start with the smallest useful bench, read the real source and
confirm every asserted fact first, and end with a decision or a named go/no-go.
