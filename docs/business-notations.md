# Business notations

The public library contains nine Neon Law review drafts. The drafts use shorter sentences, explicit commercial
schedules, typed parties and dates, and a required lawyer-review step. They do not represent an attorney’s approval of a
particular transaction. One Privacy Policy covers cookies, with U.S. state and GDPR supplements as Clauses. Website
terms are in the firm's own [Terms of Service](../neon/content/terms.md), not a template.

## Deliberate drafting choices

**NDAs.** Shorter confidentiality provisions; negotiated duration; disputes go to AAA arbitration
(`dispute_resolution@1`); protected reports preserved; no automatic feedback license or bond waiver.

**Advisors.** Compensation, vesting, and prior materials require completion; equity requires separate approval;
publicity requires consent; no assumed exclusivity.

**Employment.** The offer letter applies in any U.S. state. California rules (current thresholds, actual duties,
fair-chance requirements, separate notices) are a Clause, `california_employment@1`, included only when California is
the work state; other states need their own review. Disputes go to AAA employment arbitration
(`employment_arbitration@1`) with statutory carve-outs. Invention assignment and required notices remain separate
documents.

**MSA.** Generic services and orders replace product-specific AI infrastructure. Service levels, liability, and exit
terms require completion. Disputes go to AAA arbitration. Model training requires separate authorization.

**HIPAA.** Explicit safeguards, incident reporting, individual-rights assistance, subcontractor duties, HHS access, and
return or destruction. Confirm any additional health-data laws.

**DPAs.** Actual processing, security, and subprocessor schedules must be completed. The global version requires
executed transfer instruments and annexes; its summary is not a substitute for the SCCs or UK instrument.

**Privacy and cookies.** One policy with a cookie section. The U.S. state disclosures (`privacy_us_states@1`) and the
GDPR supplement (`privacy_gdpr@1`) are Clauses, each included only when its yes/no answer is yes. Actual practices must
be supplied and tested. No vendor, tracking consent, opt-out implementation, or age policy is assumed.

**Website terms.** The Terms of Use provisions on accounts, content, third parties, suspension, and general terms are in
the firm's Terms of Service. Disputes go to AAA arbitration; no class waiver or broad third-party release is imposed.

The drafting review uses the Legal Council’s accountability and enforceability lenses. The Client Council’s access and
clarity lenses favor short sentences and named choices. These review lenses do not replace a licensed lawyer’s approval.

## Runtime contract

The public catalog and preview list share `BUSINESS_NOTATIONS` in `neon/src/firm_pages.rs`. The canonical seed installs
the nine templates. Each paired workflow spec must match its template frontmatter. Existing composition tests check that
every seeded code has a walkable questionnaire and that each public template has one card and one preview.

Parties use `entity` or `person` states, dates use `custom_datetime`, notice contacts use `people`, and governing law
uses `jurisdiction`. The `business_*` text roles are negotiated provisions or factual schedules, explicitly listed in
rule N117. They are lawyer-facing. They do not create a second store of client identities.

## Home-page claims

The user supplied the fee model, Bay Area roots, access-to-justice and technology-community activity, Justice Tech
Association membership, Rust NYC connection, independent funding, and pro bono commitment. No comparative savings
percentage or pro bono allocation is asserted. The Rust claim is limited to memory-safety benefits and the repository’s
actual checks and review workflow. The user supplied the production-engineering statement as firm positioning. Existing
trust-deposit and page-limit disclosures remain pending any separate change to engagement terms.

The user also confirmed 12 years of experience, nine figures in exits, and coverage of the agreements in the public
library. The company-counsel catalog carries those aggregate facts without identifying clients, assigning a precise exit
value, or promising a result. The service directory describes the firm's work beyond the nine public template drafts.

Ferris is Karen Rustad Tölva’s public-domain artwork from [Rustacean.net](https://rustacean.net/), served locally as
SVG.
