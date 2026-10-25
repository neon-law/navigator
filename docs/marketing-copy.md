# Marketing copy

The firm's public home, practice, and marketing pages publish English only. The words live in
`neon/locales/en/<brand-key>/*.yaml`, one directory per [`views::brand::BrandKey`](../views/src/brand.rs). Editing them
is a YAML change: the Rust loaders pick the directory for the request's brand, interpolate `{site_name}` and
`{firm_email}`, fill runtime fields a catalog cannot know (hero asset URLs, CLI download archives, consultation URLs),
and inject the page types the Dioxus routers already take.

There is no translated surface. A second locale directory is a validate error (`Y002`), not a language switch. An
unknown brand-key directory is the same error.

## Customer-centered writing

Lead with the reader's needs, wishes, decisions, and next step. Address the reader as "you" and explain what they can
arrange, understand, or do with the service. Use "we" to describe concrete help rather than to introduce the firm. Keep
sentences plain and specific, with scope and fee terms close to the offer they qualify.

The shared footer carries firm identification. Marketing paragraphs should not repeat the operating firm name or an
"Attorney responsible for this page" sentence unless a specific required disclosure belongs there. Preserve relevant
service details, such as identifying a person who may accept an individual trustee appointment.

## Catalog files

Neon Law (`neon/`):

| File | Page |
| --- | --- |
| `neon/locales/en/neon/home.yaml` | `/` |
| `neon/locales/en/neon/litigation.yaml` | `/disputes` |
| `neon/locales/en/neon/fractional-gc.yaml` | `/business` |
| `neon/locales/en/neon/personal-plan.yaml` | `/personal` |
| `neon/locales/en/neon/navigator.yaml` | `/navigator` |
| `neon/locales/en/neon/notations.yaml` | `/notations` format introduction |
| `neon/locales/en/neon/services.yaml` | `/services` |
| `neon/locales/en/neon/team.yaml` | `/team` and Nick's short `/team/nick` profile |
| `neon/locales/en/neon/gateway-delete-your-debt.yaml` | `/delete-your-debt` — Neon-only gateway to DeleteYourDebt.com |

The shared catalog sits beside the brand directories rather than inside one, because it is shared across brands:

| File | Role |
| --- | --- |
| `neon/locales/en/shared.yaml` | Sentences more than one page or brand publishes |

DeleteYourData.com (`delete-your-data/`):

| File | Page |
| --- | --- |
| `neon/locales/en/delete-your-data/home.yaml` | `/` |
| `neon/locales/en/delete-your-data/services.yaml` | Router copy; `/services` returns 404 |

Lawyer Shook (`lawyer-shook/`):

| File | Page |
| --- | --- |
| `neon/locales/en/lawyer-shook/home.yaml` | `/` practice cards beneath the firm's notice |
| `neon/locales/en/lawyer-shook/services.yaml` | `/services` (kept valid; the route is not published on that host) |

Guieb & Shook (`guieb-shook/`):

| File | Page |
| --- | --- |
| `neon/locales/en/guieb-shook/home.yaml` | `/` page-shell defaults; the statement is hardcoded in `firm_pages.rs` |
| `neon/locales/en/guieb-shook/services.yaml` | `/services` (kept valid; the route is not published on that host) |

Daybridge Divorce Law (`daybridge/`):

| File | Page |
| --- | --- |
| `neon/locales/en/daybridge/home.yaml` | `/` agreed divorce for couples |
| `neon/locales/en/daybridge/services.yaml` | `/services` scope, timing, and cost disclosures |

Death & Divorce (`death-and-divorce/`) publishes no prices:

| File | Page |
| --- | --- |
| `neon/locales/en/death-and-divorce/home.yaml` | `/` divorce and probate |
| `neon/locales/en/death-and-divorce/services.yaml` | `/services` divorce, probate, and Vesta referral |

Daybridge serves couples who agree for a one-time $5,000 fee for a mutual split. Its public pages present that single
fee. Joint representation is subject to applicable rules, conflict review, and informed written consent. Its pages
explain shared information, independent advice, and stopping joint work when a conflict prevents it. Contested
proceedings sit outside the joint engagement. Agreement and consent alone do not establish eligibility for joint
representation.

[Nevada conflict rules](https://www.leg.state.nv.us/courtrules/rpc.html) restrict claims between clients in the same
proceeding. The copy preserves that limit.

Abhaya's service directory covers work visas, family and employment-based permanent residence, citizenship, consular
processing, employer sponsorship, and RFE/NOID responses. Its category inventory draws on the requested [Manifest
service directory](https://manifestlaw.com/), with visa descriptions checked against the Department of State's
[temporary worker](https://travel.state.gov/content/travel/en/us-visas/employment/temporary-worker-visas.html) and
[employment
immigration](https://travel.state.gov/content/travel/en/us-visas/immigrate/employment-based-immigrant-visas.html)
guides. Gold Card work begins with review of current availability and rules. Former-officer reviews and pro bono
programs require confirmation of Abhaya's own offering before they can be advertised.

DeleteYourDebt.com introduces PandA Law's debt-relief work in Nevada, Washington, New York, and California. Its primary
action opens [PandA Law's contact page](https://www.pandalawfirm.com/contact/). The team works across the country and
offers in-person meetings in Brooklyn. Guieb & Shook PC remains the site's operator; the written engagement identifies
the lawyers, scope, and fees. The homepage, services catalog, Neon gateway, and brand descriptions share this scope and
make no outcome promise. The two-column homepage uses Public Sans and the registered green palette.

Lawyer Shook's `/` combines the firm notice and client sign-in written in Rust
(`neon::firm_pages::lawyer_shook_holding_content`) with Neon Law and Vesta practice cards from its home catalog. It uses
the shared footer treatment used by Neon Law and Vesta, including the link to Guieb & Shook PC and the common closing
line.

Neon's `team.yaml` keeps the directory to one name and link; Nick's profile holds a two-sentence introduction and his
LinkedIn link. The profile does not repeat the directory's heading or copy a full service biography.

Vesta's `home.yaml` leads with support in life and death and welcomes the person's values and spiritual beliefs. It
explains that Nevada Asset Protection Trust planning may fit after reviewing the person's circumstances and applicable
law. Basic estate plans for one person start at $1,111, with services and fees agreed in writing. The package includes a
will, financial power of attorney, health-care directive, health-care power of attorney, a planning meeting, attorney
preparation and review, one round of revisions before signing, and signing guidance. Living trusts, advanced planning,
property transfers, probate, administration, and later updates require a separate quote. The home page puts an optional
astrology reading during the get-to-know-you session, followed by written engagement, the planning meeting, document
preparation, and a return meeting for signing with required witnesses and notarization. Probate has its own section and
separate scope and fees. Readings are optional personal reflection; the person's wishes guide the legal plan. The
booking link uses Vesta's `consultation_url`, matching the Notion calendar for Neon Law. Its services page describes
estate planning, Nevada trust planning, custom trust administration, and possible individual trustee appointments under
written terms. Navigator access is not an appointment, and the copy preserves the Nevada-qualified-trustee, tax,
conflict, and execution-formality limits. Blockchain recording is fulfilled on request while the automated backend in
[`solana-attestation`](solana-attestation.md) remains unshipped; a record does not replace required signing or
notarization.

Neon's estate-planning gateway and the Lawyer Shook practice card introduce Vesta's estate planning and Nevada trust
work. The gateway directs visitors to Vesta for the service details and written engagement terms.

Source review dated October 13, 2026: [Simon Law Group's process
guide](https://www.simonattorneys.com/nj-trust-estate-planning-process-timeline) separates the consultation, drafting,
review, and signing stages. [Jeffrey Burr's planning
overview](https://jeffreyburr.com/proper-estate-planning-is-a-process-not-an-event/) connects document preparation with
later administration. These inform the page structure; Vesta's meeting sequence, optional readings, and basic-plan
starting price follow its own offering. The [Nevada courts' probate
overview](https://selfhelp.nvcourts.gov/pro/overview) supports the distinction between probate help and court
proceedings required for a particular estate.

### Vesta Nevada trust planning

Nevada's [trustee rule](https://www.leg.state.nv.us/NRS/NRS-166.html#NRS166Sec015) requires a qualified Nevada trustee
when the settlor is a beneficiary and requires Nevada administration for the relevant Nevada nexus. Its [trust creation
rule](https://www.leg.state.nv.us/NRS/NRS-166.html#NRS166Sec040) requires an irrevocable writing and prohibits an intent
to hinder, delay, or defraud known creditors. Its [creditor challenge
rule](https://www.leg.state.nv.us/NRS/NRS-166.html#NRS166Sec170) addresses transfers. The public copy therefore makes no
blanket protection claim and treats planning as a custom engagement.

[Nevada Rules of Professional Conduct 5.5 and 7.1–7.4](https://www.leg.state.nv.us/courtrules/rpc.html) shape the
national reach claim: the firm may consult with people across the country, but each matter requires a review of where
the lawyers may practice and which jurisdiction's law applies. The copy states the practice focus without claiming
certified specialization. Firm identification appears in the shared footer, and scope is set in writing.

### Vesta administration

Vesta's home and services pages introduce [Estate Administration](glossary/estate-administration.md) and [Trust
Administration](glossary/trust-administration.md) through contact and a custom written engagement. The home page places
administration after the Nevada trust explanation. Administration and an individual trustee appointment each have
separate scope and terms.

Source review dated September 28, 2026: [Jeffrey Burr](https://jeffreyburr.com/trust-administration/) groups its work
around successor-trustee guidance and carrying out the trust. [Short &
Stevens](https://shortandstevenslaw.com/services/trust-administration/) makes accounting, notices, communication, and
distributions legible to visitors. These inform the service organization; Vesta's catalog makes its own scoped offer,
without adopting competitor experience or outcome claims.

[NRS 669.080(1)(c), (n)](https://www.leg.state.nv.us/NRS/NRS-669.html#NRS669Sec080) exempts performance of attorney
duties and individual fiduciary service from that chapter. This supports the advertised legal-advice and
individual-appointment paths; it does not establish that Guieb & Shook PC is a licensed trust company or qualified for
every proposed trust. [NRS 162.310](https://www.leg.state.nv.us/NRS/NRS-162.html#NRS162Sec310) distinguishes a
fiduciary's attorney from duties to the principal arising solely from that representation. The engagement must identify
the client.

The [Nevada Rules of Professional Conduct](https://www.leg.state.nv.us/courtrules/rpc.html) supply the ethics limits:

- Rules 1.1–1.5: competence, diligence, communication, defined scope, and reasonable fees.
- Rules 1.6–1.10: confidentiality and current, former-client, personal-interest, and firm conflicts. Consent cures only
  consentable conflicts; Rule 1.7(b) requires informed consent confirmed in writing and its other conditions.
- Rule 1.8(a): where a related transaction falls within the rule, fair written terms, advice to obtain independent
  counsel, an opportunity to do so, and signed informed consent are required.
- Rule 1.15: separate and safeguard property, keep records, and account for funds received in representation.
- Rules 7.1, 7.2, and 7.4: accurate service claims, a named responsible lawyer, and proper fee and specialization
  disclosures. Rule 7.2A excludes websites from advertisement filing; separate campaigns need their own analysis.

Individual appointments require trust-specific authority, eligibility, tax, compensation, and conflict review. The
catalog promises that review and separate written terms; it grants no appointment or blanket ethics clearance. The
existing qualified-Nevada-trustee and life-insurance limitations remain in the services catalog.

Vesta's `/probate` page introduces probate and estate administration after a death, with a link in Vesta's footer. It
offers practical first steps and separate written scope and fees, with a source link to the Nevada courts. The
estate-plan fee note starts with "Fees"; the price itself appears once in the offer heading.

Vesta is registered by the canonical brand seed on each deployment boot, including staging's persistent runtime profile.
Its page and SVG ship with the application. The disposable estate fixture belongs to Vesta when the sample portfolio is
explicitly seeded; a staging restart does not recreate that portfolio.

Aegis Criminal Defense (`aegis/`) and Groundwork Real Estate Law (`groundwork/`) each ship `home.yaml`, `services.yaml`,
and three explainer catalogs. Aegis uses black and yellow, DM Sans body text, Barlow Condensed headings, and an original
shield graphic inspired by hip-hop poster design. Groundwork uses Source Serif 4, limestone and oxblood red, and an
original SVG illustration of a French double door. The illustrations are tracked static assets.

Both header and footer Contact links and the home, services, and explainer booking buttons use the firm's existing
Notion calendar. The local contact page and lead form remain available. Copy invites a matter review and confirms
jurisdiction, conflicts, scope, and fees before representation. Neither catalog promises an outcome or publishes an
unverified fee.

The three explainer routes are `/explainers/first-steps`, `/explainers/process`, and `/explainers/documents`. Aegis
covers investigation preparation, federal criminal procedure, and record review. Groundwork covers purchase preparation,
lease review, and property-dispute preparation. Each brand's catalog and crawler index publish its own explanations;
other brands return 404 for these routes.

Source review dated October 1, 2026: [Shouse Law Group](https://www.shouselaw.com/) and [Spodek Law
Group](https://www.federallawyers.com/) make case categories, explanations, and contact paths easy to find. These inform
navigation rather than firm experience or outcome claims. The federal-process guide cites [U.S.
Courts](https://www.uscourts.gov/about-federal-courts/types-cases/criminal-cases) and explicitly distinguishes state
procedure. Groundwork's mortgage-disclosure explanation cites the [CFPB Closing Disclosure
guide](https://www.consumerfinance.gov/owning-a-home/closing-disclosure/). The other guides organize questions and
documents for a review without supplying jurisdiction-specific deadlines or conclusions.

Their purchased domains are registered for `www` and `staging` hosts and enter the release certificate inventory. They
stay outside `BrandKey::LIVE` until DNS and TLS readiness are verified. Analytics is unconfigured, so neither site loads
another brand's tracking script. Local previews use `NAVIGATOR_LOCAL_AEGIS_PORT` and `NAVIGATOR_LOCAL_GROUNDWORK_PORT`.
Public family links appear when the launch gate admits each brand. Lawyer Shook's holding-page directory lists every
registered house brand, including Aegis and Groundwork, so the portfolio stays complete while those hosts follow the
launch gate.

Which stems a key ships is [`BrandKey::catalog_pages`](../views/src/brand.rs). DeleteYourData.com publishes its annual
product on `/` and office details at `/contact`; `/services` returns 404. Its services catalog is loaded when the shared
router builds its brand map. Lawyer Shook answers `/` alone. Other firm paths 404 on that host.

`views::locales` is the typed schema. `navigator project gate` deserializes each file as the page its stem names, so a
missing field or an unknown stem fails the gate before a brand crate can load it. The advertising guards in
`neon::firm_copy` still read the loaded Neon pages when the Rust suite runs.

## The shared catalog

Some sentences are published by more than one page or brand. Those sentences are authored once, in
`neon/locales/en/shared.yaml`, and nowhere else.

```yaml
catalog_version: 1
entries:
  litigation.title: Your story deserves to be heard.
brands:
  delete-your-data:
    litigation.title: Ask them to delete it.
```

A page catalog references a key instead of repeating the words:

```yaml
heading:
  text: "{shared:litigation.title}"
```

`{shared:<key>}` resolves before `{site_name}` and `{firm_email}`, in the raw YAML, so a reference works in any string
field without the page schema knowing about it. A shared value may itself carry the two brand placeholders.

[`views::locales::shared`](../views/src/locales/shared.rs) is the contract, and `navigator project gate` (`Y002`)
enforces all of it:

- **`catalog_version` equals the version this build authors.** A consumer built for another version refuses the
  document rather than rendering half of it.
- **Every key in `REQUIRED_KEYS` is present.** Required copy has no fallback: a missing headline fails the build
  instead of publishing a gap.
- **A value is one line, non-empty, and carries no double quote.** Values are substituted into quoted YAML scalars.
- **A value's only placeholders are `{site_name}` and `{firm_email}`.** Anything else reaches a reader as a literal
  brace.
- **A brand override names a key the shared defaults define.** A brand cannot smuggle in a key no default carries.

**Fallback.** `SharedCatalog::lookup(brand_key, key)` reads that brand's override under `brands:` and otherwise the
shared default under `entries:`. It never reads a different brand's override, so a brand that has not overridden a
sentence publishes the shared one rather than a sibling brand's wording.

Copy that differs between pages today is deliberately **not** in the shared catalog. This catalog removes duplication;
it does not decide wording. Wording lives with the page-copy reviews.

Coverage: [`server/tests/firm_routes.rs`](../server/tests/firm_routes.rs) proves each served page renders the sentence
the catalog authors, and that no page leaks an unresolved placeholder.

A home catalog may carry an optional `provenance` block with a request flow, ledger illustration, tiles, and notes. The
generic home renderer supports it; the current practice catalogs omit it.

DeleteYourData's `privacy` block presents data removal as a non-legal service, with monthly and annual billing — $29/mo
or $290/year prepaid — and basic form filings such as forming an LLC included in that price. Government fees are
separate. Gift cards are available, and privacy and credit monitoring requires opt-in at enrollment. Its
`sibling_practices` cards point at two sibling practices: the optional [Death Check](glossary/death-check.md) through
Vesta Estate Planning, which sets the triggers written into a client's Vesta estate plan in motion while their
subscription is active, and cyber-crime claims through Cyber Injury Law. Vesta's `estate` and Cyber Injury Law's
`cyber_injury` blocks carry a `sibling_practices` card pointing back at DeleteYourData.com. Each card names its
destination by brand key; the address is resolved per deployment, and a card for a brand that is not live is dropped.
Signup and gift-card links use the same Notion booking calendar as Neon and Vesta. Plus Jakarta Sans is used throughout
the page and footer. Pricing uses the shared `PricingSection` with the brand palette and typeface. The cloak crossing an
internet grid is decorative; reduced-motion preferences disable animation. The Solana section carries the approved
request-record and litigation copy. The page uses the same footer treatment as Neon and Vesta. The subscription is also
called [Neon Law Nautilus](glossary/neon-law-nautilus.md). Neon publishes `/nautilus`
(`neon/locales/en/neon/nautilus.yaml`) under the one name Nautilus, with no themed card names. An `app_preview` band
draws the coming iPhone and Android apps in each platform's idiom, listing every check as an equal peer: data removal,
privacy protection, credit monitoring, dark web monitoring, identity theft insurance, unclaimed property, estate
planning, and basic form filings. Its listing price, like the $29/mo and $290/year pricing cards, resolves from
DeleteYourData's home catalog. The store badges read "Coming soon" and link nowhere until the apps are published. A
lawyer review starts at $20 and is prepaid; the private legal sandbox includes $5 of model usage a month, then is priced
à la carte by Lex Tecnica. Monitoring and insurance coverage are confirmed before activation. Its CTA opens a three-step
public contact intake before sign-in; the signed-in page exposes the configured enrollment booking link. DocuSign
paperwork follows the enrollment review. The page expressly distinguishes privacy from attorney-client privilege. Every
other brand host answers `/nautilus` with a 404.

The Neon home page presents common employment, NDA, sales, and privacy agreements under a $5,000 one-time contract fee;
financings are scoped separately. It also presents a $50 daily minimum for questions over Slack and a separate $500
expedited review per contract. The expedited review gives the client a practical next step within one business day. Each
contract covers up to 50 US letter size pages in Times New Roman 10pt or larger, with each additional page priced at $5.
Its `company` block also holds the service directory, Apache-licensed Rust explanation, independent-funding and
community copy, and notation callout. GORP and cyan remain the identity; the layout uses static illustrations and
generous spacing. An interactive trust-balance chart shows the earned contract fee, daily counsel, and review fees
against the deposit. Its brief allocation animation respects reduced-motion preferences. The presentation video remains
user-controlled. The business template inventory and drafting choices are documented in [business
notations](business-notations.md).

Neon publishes one offer on `/`, plus its fee schedule on `/services`; `/business` and `/disputes` return 404. Other
brands retain their own home layouts. Litigation and sibling-practice scope remain separately disclosed.

## A copy-only pull request

Change the YAML, then run:

```bash
cargo run -p cli --quiet -- project gate
```

CI always runs that command. It skips `cargo test --workspace` when the PR touches no Rust sources. A schema change
belongs in `views::locales` and is a Rust change.

The catalog is compiled into the brand crate with `include_str!`. A merged YAML edit lands on the next image build that
compiles `neon`.

Every registered house brand publishes a Lead form on its home page. Neon keeps its contact dialog and Summons keeps its
intake form; the other layouts show the shared form inline, including Cyber Injury and Lawyer Shook. Each form posts to
`/leads` with the request's CSRF token and source path. Email is required, phone is optional, and SMS consent is a
separate choice. The route test submits a synthetic inquiry from every live brand and verifies its attribution.

Misericordia focuses on compassionate personal injury representation: vehicle collisions, premises injuries,
catastrophic injuries, wrongful death, elder care abuse and neglect, and medical negligence. Cyber Injury Law reviews
the same injury categories for a different audience, and also reviews data theft, cybercrime, digital stalking, and
online abuse. Its campaign line is "We use AI to put more money in your pocket." DeleteYourData introduces Cyber Injury
Law for review of harm from a data leak or misuse, while keeping data removal as its primary offer. Recording a removal
request does not promise compliance or prevent a future leak.

See [`gate.md`](gate.md) for `Y002` and [`gitops.md`](gitops.md) for the conditional rust job.

Summons Defense publishes its OATH hearing offer on `/` and `/services`: $100 per telephone hearing and a one-time $500
in-person appearance fee. Its `summons` home block carries the intake process, expandable agency preparation guides,
public OATH appeal examples, and government-only resource links. The home form uses the shared Lead capture handler to
request contact; document collection and assistant-led intake follow separately. The page does not run an AI intake
session or accept document uploads. `/contact` provides the shared contact surface.

Neon's `/services` brand cards resolve their registered logos through the shared asset resolver. The directory lists
every house brand except Neon Law and Lawyer Shook, and those cards carry no practice-area pills. Website design uses
descriptive labels without decorative zero-padded numbering; see
[website-style](../.agents/skills/website-style/SKILL.md).
