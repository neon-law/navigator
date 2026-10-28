# Clauses

A Clause is a reusable block of legal prose. The glossary defines words; the Clause library supplies wording. A Template
chooses a pinned source and binds its inputs to typed questionnaire states. Composition happens before persistence and
before preview or rendering. The document renderer then uses its ordinary placeholders, conditions, and iterators.

## Include a Clause

Declare an alias in the notation's frontmatter and reference it in the body:

```yaml
kind: agreement
clauses:
  law:
    source: dispute_resolution@1
    inputs:
      law: jurisdiction__law
      seat: jurisdiction__arbitration_seat
```

```markdown
## VI. Administration

{{> law}}
```

The notation must declare a questionnaire. Required input states absent from that questionnaire are appended before
`END`, in alias and input-name order. Existing states, prompts, audiences, and choices retain their authored settings.
Missing prompts receive the Clause's prompt. A jurisdiction input binds a jurisdiction answer; it does not establish
that the chosen wording is appropriate in every jurisdiction.

The compiled snapshot carries the expanded body and `clause_sources:` provenance with the source version and bindings.
It contains no live library reference. Project imports persist that source snapshot; shared catalog seeding persists its
expanded body through the existing asset seam. Answers continue to live in the Notation's questionnaire. A library
update must add a new source version; changing a reference creates a new Template version through the existing
persistence seam.

## Retire a version

Set `retired_at` on a library version when it must stop accepting new references. The date is copied to the PostgreSQL
read model on the next full `navigator-migrate` run. Retirement preserves the version, inputs, wording, digest, and
insertion timestamp. Re-seeding is idempotent and never reactivates a retired row.

A retired version remains readable through `clause show` and the Clause library pages, where it is marked retired. It is
never current. `N132` and composition reject new `clauses:` references to it. Saved snapshots carry expanded wording and
`clause_sources:` provenance, so they remain readable and render without resolving the library again.

`governing_law@1` and the JAMS version `nevada_engagement_arbitration@1` are retired. Use an active version with the
appropriate legal effect and document context for new Templates.

## Arbitration inputs

The existing onboarding draft's arbitration wording is available as `engagement_arbitration@1`. It is restricted to
`kind: onboarding` and has separate `law` and `seat` inputs. This extraction preserves the draft's administrator, rules,
one-arbitrator requirement, exceptions, and disclosures; it does not establish approval for a new jurisdiction or
document context.

```yaml
kind: onboarding
clauses:
  arbitration:
    source: engagement_arbitration@1
    inputs:
      law: jurisdiction__governing_law
      seat: jurisdiction__arbitration_seat
```

Every `kind: onboarding` template must declare the highest active version of `engagement_arbitration` under `clauses:`
and place it in the body with `{{> name}}`; `N132` (and so `navigator project gate`) fails the template otherwise. The
onboarding scaffold stub starts with the clause, its `clauses:` block, and the two jurisdiction states. An offboarding
letter ends an engagement already governed by the signed onboarding letter, so the requirement does not apply to it.

The body references `{{> arbitration}}`. Governing law, arbitration seat, court forum, and administrator are distinct
decisions. The Clause accepts a jurisdiction state or an existing single-choice state for its law and seat. A
single-choice binding still needs the notation's explicit choices. Dispute resolution is always American Arbitration
Association (AAA) arbitration. `dispute_resolution@1` serves agreements under the Commercial Arbitration Rules,
`employment_arbitration@1` serves offer letters under the Employment Arbitration Rules with statutory carve-outs, and
`nevada_engagement_arbitration@2` serves the Nevada engagement letter. The earlier JAMS wording stays as
`nevada_engagement_arbitration@1` for history and is not used.

## Conditional supplements

A Clause may wrap its prose in `{{#if applies}}…{{/if}}` and take a `custom_yes_no` input named `applies`, so a notation
includes it only when the answer is yes. `privacy_us_states@1` and `privacy_gdpr@1` work this way in the Privacy Policy:
the U.S. state and GDPR supplements appear only when the drafter says those laws apply. `california_employment@1` is
included in the Employee Offer Letter when the work state is California.

When a choice selects one of several bodies, use `{{else if state=value}}` branches and an optional `{{else}}` before
the closing `{{/if}}`. The first matching branch renders; `{{else}}` renders when no earlier condition matches. For a
custom choice state, `N115` requires every option in `choices.<role>` to have a branch or an explicit `{{else}}`
fallback in that conditional. It also rejects branch values absent from the declared options, including when a fallback
exists. Nested and separate conditionals each need their own coverage.

```text
{{#if custom_single_choice__category=first}}First body.
{{else if custom_single_choice__category=second}}Second body.
{{else}}Fallback body.
{{/if}}
```

## Find the wording

```bash
cargo run -p cli --quiet -- clause list
cargo run -p cli --quiet -- clause show 'governing_law@1'
```

The versioned source of truth is [`rules/clauses/library.yaml`](../rules/clauses/library.yaml). When PostgreSQL is
configured, `clause list` and `clause show` read the shared `clause` table; when it is not configured or unavailable,
they read the library embedded in the binary. `list` reports each version's kind and status: current, not current, or
retired. The table is seeded from the bundled library by `navigator-migrate`; edits add a version to the YAML source.
`N132` rejects unknown or retired versions, missing or extra inputs, incompatible input types, wrong notation kinds,
unused declarations, and unresolved references. Expanded prose also passes the placeholder and drafting-marker rules.
Missing answers remain visible placeholders in previews and are refused by the existing final-document check.

## Browse the library

The lawyer tier reads the library at `/app/admin/clauses`. The list shows each Clause's current version, or its latest
retired version when none is current. Each row shows the kind, input count, retirement status, and how many shared
Templates pin any version of it. The search box matches a code, a kind, or wording in the body; the kind menu filters
exactly. `/app/admin/clauses/{code}` shows every version's body and inputs and the Templates that pin that version.
Usage is read from each shared Template's `clause_sources:` snapshot, so a Project's own Templates do not appear. The
canonical seed stores a Template's body without its frontmatter, so for a seeded body the snapshot is recovered from the
bundled source it was seeded from (`store::seed::seeded_clause_sources`), and only when the stored body is
byte-identical to it. The pages read the same catalog as `clause list` and write nothing: a change is still a new
version in the YAML.

## Extract more Clauses

Use [extract-clauses](../.agents/skills/extract-clauses/SKILL.md) to identify provision boundaries, bind decisions, and
prove extraction preserves the rendered body. Matter documents inform private analysis; their text is not a public
library source.

## Common wording survey

The shared catalog has thirty legal templates. Similar headings identify candidates, not interchangeable obligations.

| Family | Evidence | Decision |
| --- | --- | --- |
| Governing law and disputes | NDAs, advisor agreement, MSA | One AAA clause, `dispute_resolution@1`. |
| General administration | NDAs, advisor agreement, MSA, BAA, DPAs | Keep context-specific sources. |
| Arbitration | Onboarding and Nevada engagement | Keep dispute contexts separate. |
| Confidentiality and disclosure | NDAs, advisor agreement, MSA | Compare obligations before sharing. |
| Processing and security | U.S. and global DPAs | Preserve transfer differences. |
| Collection, sharing, rights | U.S. and GDPR privacy policies | Preserve territorial differences. |
| Signatures and acceptance | NDAs, advisor, MSA, BAA, DPAs, offer | Keep context-specific sources and fixed labels. |
| Employee offer terms | Employee offer letter | Extract with law and notice-contact inputs. |
| Scope, conflicts, termination | Engagement and onboarding | Preserve fee and representation effects. |

The NDA administration paragraph covers entire agreement, amendments, waiver, severability, assignment, and
counterparts. The advisor, MSA, BAA, and DPA provisions remain separate because they carry different terms. Their
sources are `advisor_administration@1`, `msa_administration@1`, `baa_administration@1`, and `dpa_administration@1`;
offer terms are in `employee_offer_terms@1`. The NDAs use `nda_signatures@1`; both DPAs and the MSA share
`provider_customer_signatures@1`. The advisor and BAA use `advisor_signatures@1` and `baa_signatures@1`; offer
acceptance uses `employee_acceptance@1`. NDA party names remain typed entity inputs; fixed signature labels remain
literal.

The NDAs, advisor agreement, and MSA use `dispute_resolution@1`. The onboarding and Nevada engagement templates now
include their distinct arbitration Clauses. The onboarding retains its existing binding of both law and seat to the
governing-law answer. The Nevada engagement uses `nevada_engagement_arbitration@2`, with AAA rules and Nevada
fee-dispute protection. The employment offer letter uses `employment_arbitration@1` and includes
`california_employment@1` only when California is the work state.

Reserved-heading enforcement, automatic jurisdiction variants, nested Clause references, and automatic extraction of
similar text are separate work. The library makes reuse explicit; it does not infer legal equivalence from similar
words.
