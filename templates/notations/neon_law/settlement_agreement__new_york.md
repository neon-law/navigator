---
kind: agreement
title: Settlement Agreement
respondent_type: person
jurisdiction: NY
confidential: true
signers:
  - client
  - spouse
clauses:
  property:
    source: divorce_property_division@1
  maintenance:
    source: divorce_maintenance@1
    inputs:
      applies: custom_yes_no__maintenance_applies
  parenting:
    source: divorce_parenting@1
    inputs:
      applies: custom_yes_no__minor_children
  signatures:
    source: divorce_signatures@1
    inputs:
      spouse_one: person__client
      spouse_two: person__spouse
prompts:
  index_number: What index number has the clerk assigned?
  marriage_date: On what date were you married?
  place_of_marriage: Where were you married (city and state)?
  county_of_residence: In which county will the action be filed?
  maintenance_applies: Will one spouse pay maintenance to the other?
  child_support_weekly: What weekly child support amount have the parties agreed on?
  minor_children: Are there minor children of the marriage?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__spouse
  person__spouse:
    _: custom_datetime__marriage_date
  custom_datetime__marriage_date:
    _: custom_text__place_of_marriage
  custom_text__place_of_marriage:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: custom_text__index_number
  custom_text__index_number:
    _: custom_yes_no__minor_children
  custom_yes_no__minor_children:
    _: schedule__children
  schedule__children:
    _: custom_usd__child_support_weekly
    columns:
      - name: child
        label: Child's name
        type: text
      - name: birth_date
        label: Date of birth
        type: date
  custom_usd__child_support_weekly:
    _: schedule__property_division
  schedule__property_division:
    _: schedule__maintenance_terms
    columns:
      - name: property
        label: Property or debt
        type: text
      - name: disposition
        label: Who takes it, or who pays it
        type: text
  schedule__maintenance_terms:
    _: schedule__parenting_time
    columns:
      - name: item
        label: Term
        type: text
      - name: detail
        label: Amount, frequency, duration, and stopping events
        type: text
  schedule__parenting_time:
    _: END
    columns:
      - name: item
        label: Day or occasion
        type: text
      - name: detail
        label: Parenting arrangement
        type: text
  END: {}
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    approved: generate_pdf__settlement
    changes_requested: reask__client
    rejected: END
  reask__client:
    intake_resubmitted: lawyer_review
  generate_pdf__settlement:
    pdf_persisted: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
    signature_declined: END
  END: {}
---

# Settlement agreement

**Plaintiff.** {{person__client.name}}

**Defendant.** {{person__spouse.name}}

**Index No.** {{custom_text__index_number}}

**Married.** {{custom_datetime__marriage_date}}

**Married at.** {{custom_text__place_of_marriage}}

**Filing county.** {{custom_text__county_of_residence}}

## Recitals

The parties married on {{custom_datetime__marriage_date}}.

They married at {{custom_text__place_of_marriage}} and separated after that date. They have resolved every issue between
them — the property, the debts, the support, and the parenting of their children — and intend that this agreement
survive as a contract and be incorporated without being merged into the judgment of divorce. Each spouse has had the
opportunity to consult independent counsel of that spouse's choosing.

## I. Property and debts

{{> property}}

### A. Property division

{{#for row in schedule__property_division}}

- {{row.property}}: {{row.disposition}}.

{{/for}}

## II. Maintenance

{{> maintenance}}

### A. Maintenance terms

{{#for row in schedule__maintenance_terms}}

- {{row.item}}: {{row.detail}}.

{{/for}}

## III. Parenting and child support

The parties' minor children:

{{#for child in schedule__children}}

- {{child.child}}, born {{child.birth_date}}.

{{/for}}

{{> parenting}}

Child support is {{custom_usd__child_support_weekly}} per week, computed under the Child Support Standards Act, unless
the schedule above records the parties' agreed basis for a different amount.

### A. Parenting time

{{#for row in schedule__parenting_time}}

- {{row.item}}: {{row.detail}}.

{{/for}}

## IV. Execution

{{> signatures}}

Each spouse signs where the table above directs, and the signature tabs land on these anchors: {{client.signature}} and
{{spouse.signature}}.
