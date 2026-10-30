---
kind: will
title: Basic Last Will and Testament
jurisdiction: US
respondent_type: person
confidential: true
prompts:
  law: "In which state is the testator domiciled? That state's law governs this will."
  testator: "Who is the testator, the person making this will?"
  executor: "Who is the executor, the person who administers the estate?"
  alternate_executor: "Who is the alternate executor, if the executor cannot serve?"
  guardian: "Who should raise the testator's minor children if both parents are gone?"
  beneficiaries: "Who are the beneficiaries? List every person who inherits under this will."
  specific_bequests: "List each specific gift, one row per gift: who receives it and what it is."
  self_proving: "Should the will include a self-proving affidavit?"
questionnaire:
  BEGIN: { _: jurisdiction__law }
  jurisdiction__law: { _: person__testator }
  person__testator: { _: person__executor }
  person__executor: { _: person__alternate_executor }
  person__alternate_executor: { _: person__guardian }
  person__guardian: { _: people__beneficiaries }
  people__beneficiaries: { _: schedule__specific_bequests }
  schedule__specific_bequests:
    _: custom_yes_no__self_proving
    columns:
      - name: beneficiary
        label: Who receives it
        type: text
      - name: gift
        label: What it is
        type: text
  custom_yes_no__self_proving: { _: END }
  END: {}
workflow:
  BEGIN: { _: lawyer_review }
  lawyer_review: { approved: generate_pdf__will_pdf }
  generate_pdf__will_pdf: { pdf_persisted: witnesses }
  witnesses: { _: _signature }
  _signature: { _: notarization }
  notarization: { _: END }
  END: {}
---

# Basic Last Will and Testament

I, {{person__testator.name}}, declare this to be my last will and testament. I revoke every prior will and codicil.

## I. Declaration

I make this will willingly and of my own sound mind. The sections below name the people I trust to carry out its terms,
the people and organizations I want to inherit from me, and the gifts I want made.

## II. Executor

I nominate {{person__executor.name}} to serve as executor of my estate. If {{person__executor.name}} is unable or
unwilling to serve, I nominate {{person__alternate_executor.name}} to serve in that order.

## III. Guardianship

If at my death any of my children is a minor, I nominate {{person__guardian.name}} as guardian of that child's person
and estate.

## IV. Beneficiary bequests

I give the rest of my estate to the beneficiaries named below. The reviewing attorney settles the shares and any
conditions before this will is released.

{{#for beneficiary in people__beneficiaries}}

- {{beneficiary.name}}

{{/for}}

## V. Specific bequests

I make the specific gifts listed below, one row per gift, as the schedule records them.

{{#for bequest in schedule__specific_bequests}}

- To {{bequest.beneficiary}}: {{bequest.gift}}.

{{/for}}

## VI. State-specific provisions

The testator's state of domicile selects the provisions that follow; exactly one state's subsection applies to this
will.

{{#if jurisdiction__law=NV}}

### A. Nevada provisions

This will is governed by the laws of the State of Nevada. Nevada Revised Statutes chapter 133 governs wills in Nevada,
and the firm's Nevada estates playbook anchors the execution and witnessing formalities this will follows. The firm's
attorneys confirm the exact statutory wording of these provisions before this will is released.

{{/if}}

{{#if jurisdiction__law=CA}}

### B. California provisions

This will is governed by the laws of the State of California. The firm's attorneys must supply this state's provisions:
this section is deliberately left for attorney drafting through the lawyer-review gate, and no statutory language is
drafted here until they supply it.

{{/if}}

{{#if jurisdiction__law=NY}}

### C. New York provisions

This will is governed by the laws of the State of New York. The firm's attorneys must supply this state's provisions:
this section is deliberately left for attorney drafting through the lawyer-review gate, and no statutory language is
drafted here until they supply it.

{{/if}}

{{#if jurisdiction__law=WA}}

### D. Washington provisions

This will is governed by the laws of the State of Washington. The firm's attorneys must supply this state's provisions:
this section is deliberately left for attorney drafting through the lawyer-review gate, and no statutory language is
drafted here until they supply it.

{{/if}}

## Execution

I sign this will on the date below, in the presence of the witnesses who attest below, at the signing ceremony the
matter records.

Date: ______________________________

Testator's signature: ______________________________

{{person__testator.name}}, Testator

## Attestation and acknowledgment

Each witness signs below, attesting to what they saw at the signing. The reviewing attorney supplies the attestation
wording this state's law asks for.

Witness's signature: ______________________________

Witness's signature: ______________________________

{{#if custom_yes_no__self_proving=Yes}}

## Self-proving affidavit

The firm's attorneys supply the self-proving affidavit in the form this state's law provides. The notarization the
matter records covers this affidavit at the signing ceremony.

Subscribed and sworn before me:

Notary's signature: ______________________________

{{/if}}
