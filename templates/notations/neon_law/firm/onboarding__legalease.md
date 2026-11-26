---
kind: onboarding
title: LegalEASE Onboarding Letter
respondent_type: person
jurisdiction: US
confidential: true
prompts:
  client: Who is the individual represented in this matter?
  engagement_scope: What work is included in this engagement?
  includes_pleadings: Does the agreed scope include signing and filing pleadings?
  governing_law: Which state's law governs this engagement?
  arbitration_seat: Where is the arbitration seated?
audiences:
  engagement_scope: lawyer
  includes_pleadings: lawyer
  cap_shape: lawyer
  member_billing: lawyer
  plan_cap: lawyer
  plan_rate: lawyer
  paralegal_rate: lawyer
  overage_rate: lawyer
  contested_cap: lawyer
  uncontested_cap: lawyer
  current_scope: lawyer
choices:
  cap_shape:
    hourly_cap: Hours cap
    dollar_cap: Dollar cap
    contested_uncontested: Contested and uncontested caps
    consult_only: Consultation only
  member_billing:
    paid_in_full: Paid in full
    overage_at_discount: Discounted overage
  current_scope:
    uncontested: Uncontested
    contested: Contested
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: project__engagement
  project__engagement:
    _: custom_text__engagement_scope
  custom_text__engagement_scope:
    _: custom_yes_no__includes_pleadings
  custom_yes_no__includes_pleadings:
    _: jurisdiction__governing_law
  jurisdiction__governing_law:
    _: jurisdiction__arbitration_seat
  jurisdiction__arbitration_seat:
    _: custom_single_choice__cap_shape
  custom_single_choice__cap_shape:
    hourly_cap: custom_usd__plan_cap
    dollar_cap: custom_usd__plan_cap
    contested_uncontested: custom_usd__contested_cap
    consult_only: custom_single_choice__member_billing
  custom_usd__plan_cap:
    _: custom_usd__plan_rate
  custom_usd__contested_cap:
    _: custom_usd__uncontested_cap
  custom_usd__uncontested_cap:
    _: custom_single_choice__current_scope
  custom_single_choice__current_scope:
    _: custom_usd__plan_rate
  custom_usd__plan_rate:
    _: custom_usd__paralegal_rate
  custom_usd__paralegal_rate:
    _: custom_single_choice__member_billing
  custom_single_choice__member_billing:
    paid_in_full: END
    overage_at_discount: custom_usd__overage_rate
  custom_usd__overage_rate:
    _: END
  END: {}
clauses:
  arbitration:
    source: engagement_arbitration@1
    inputs:
      law: jurisdiction__governing_law
      seat: jurisdiction__arbitration_seat
  technology:
    source: engagement_technology@1
    inputs:
      law: jurisdiction__governing_law
  plan:
    source: legal_plan_legalease@1
    inputs:
      cap_shape: custom_single_choice__cap_shape
      member_billing: custom_single_choice__member_billing
      plan_cap: custom_usd__plan_cap
      plan_rate: custom_usd__plan_rate
      paralegal_rate: custom_usd__paralegal_rate
      overage_rate: custom_usd__overage_rate
      contested_cap: custom_usd__contested_cap
      uncontested_cap: custom_usd__uncontested_cap
      current_scope: custom_single_choice__current_scope
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: generate_pdf__retainer_pdf
    changes_requested: reask__client
    rejected: END
  reask__client:
    intake_resubmitted: lawyer_review
  generate_pdf__retainer_pdf:
    pdf_persisted: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
    signature_declined: END
  END: {}
---

Re: Engagement for {{project__engagement.name}}

Dear {{person__client.name}}:

Thank you for choosing the Firm. This letter states the legal services we agree to provide and how they are paid.

## Scope

We represent you as an individual in this matter. We agree to do the following work:

> {{custom_text__engagement_scope}}

{{#if custom_yes_no__includes_pleadings}}
This scope includes signing and filing pleadings as described above. {{else}} This scope excludes signing and filing
pleadings and appearing in court. We will agree to any expansion of the scope in a signed writing before taking on that
work.
{{/if}}

## Plan coverage and fees

{{> plan}}

## Matter page

{{#if firm.portal_url}}
Sign in to your matter page at <{{firm.portal_url}}> to read updates and share documents with the Firm. {{else}} The
Firm provides access to your matter page for updates and documents.
{{/if}}

## Confidentiality and technology

We protect your confidential information as the law and professional rules require.

{{> technology}}

## Governing law and arbitration

{{> arbitration}}

## Ending the engagement

You may end the engagement in writing. The Firm may withdraw subject to its professional obligations and any required
court approval. We will tell you what remains to be done and how to obtain your file.

## Signatures

{{person__client.name}} \
{{client.signature}} {{client.date}}

{{firm.legal_name}} \
{{firm.signature}} {{firm.date}}
