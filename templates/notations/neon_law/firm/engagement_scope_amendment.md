---
kind: agreement
title: Engagement Scope Amendment
respondent_type: person
jurisdiction: US
confidential: true
prompts:
  engagement_date: When did the parties sign the original engagement letter?
  engagement_scope: What additional work does this amendment authorize?
  includes_pleadings: Does the additional work include signing and filing pleadings?
audiences:
  engagement_date: lawyer
  engagement_scope: lawyer
  includes_pleadings: lawyer
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: project__engagement
  project__engagement:
    _: custom_datetime__engagement_date
  custom_datetime__engagement_date:
    _: custom_text__engagement_scope
  custom_text__engagement_scope:
    _: custom_yes_no__includes_pleadings
  custom_yes_no__includes_pleadings:
    _: END
  END: {}
workflow:
  BEGIN:
    intake_submitted: lawyer_review
  lawyer_review:
    approved: generate_pdf__amendment
    rejected: END
  generate_pdf__amendment:
    pdf_persisted: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
    signature_declined: END
  END: {}
---

# Engagement Scope Amendment

## I. Additional Work

This amendment applies to the engagement letter dated {{custom_datetime__engagement_date}} between
{{person__client.name}} and {{firm.legal_name}} for {{project__engagement.name}}.

The Firm and the client agree that the engagement includes the following additional work:

> {{custom_text__engagement_scope}}

{{#if custom_yes_no__includes_pleadings}}
The additional scope includes signing and filing pleadings described above. {{else}} The additional scope excludes
signing and filing pleadings.
{{/if}}

The parties agree in writing on any additional fees and Legal Plan authorization before the Firm begins that work. The
original engagement letter governs the other terms of the engagement. This amendment takes effect when both parties sign
it.

## II. Signatures

{{person__client.name}} \
{{client.signature}} {{client.date}}

{{firm.legal_name}} \
{{firm.signature}} {{firm.date}}
