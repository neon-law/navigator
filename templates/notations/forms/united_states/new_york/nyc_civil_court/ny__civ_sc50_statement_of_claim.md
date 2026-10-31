---
kind: filing
title: Statement of Claim (Small Claims)
respondent_type: person
jurisdiction: NY
origin_url: https://www.nycourts.gov/LegacyPDFS/courts/nyc/civil/forms/CIV-SC-50.pdf
confidential: true
output: form
form: ny__civ_sc_50_small_claims_complaint
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: people__claimants
  people__claimants:
    _: person__defendant
  person__defendant:
    _: people__defendants
  people__defendants:
    _: custom_phone__defendant_contact
  custom_phone__defendant_contact:
    _: custom_usd__claim_amount
  custom_usd__claim_amount:
    _: custom_single_choice__primary_claim_reason
  custom_single_choice__primary_claim_reason:
    _: custom_datetime__claim_occurrence_date
  custom_datetime__claim_occurrence_date:
    _: custom_text__auto_accident_place
  custom_text__auto_accident_place:
    _: custom_text__claim_other_description
  custom_text__claim_other_description:
    _: custom_text__identifying_numbers
  custom_text__identifying_numbers:
    _: END
  END: {}
choices:
  primary_claim_reason:
    automobile: Damage caused to an automobile
    other_personal_property: Damage caused to other personal property
    person: Damage caused to a person
    real_property: Damage caused to real property
    proper_repairs: Failure to provide proper repairs
    proper_services: Failure to provide proper services
    proper_merchandise: Failure to provide proper merchandise
    goods_paid_for: Failure to provide goods paid for
    security_not_returned: Failure to return security
    property_not_returned: Failure to return property
    deposit_not_returned: Failure to return a deposit
    money_loaned: Failure to return money loaned
    unpaid_salary: Failure to pay salary
    unpaid_services_rendered: Failure to pay for services rendered
    insurance_claim: Insurance claim
    unpaid_commissions: Failure to pay commissions
    unpaid_rent: Failure to pay rent
    goods_sold_delivered: Failure to pay for goods sold and delivered
    warranty_breach: Breach of warranty
    agreement_breach: Breach of agreement
    contract_breach: Breach of contract
    lease_breach: Breach of lease
    use_of_property: Loss of use of property
    time_from_work: Loss of time from work
    luggage_loss: Loss of luggage
    property_loss: Loss of property
    bounced_check: Returned check (bounced)
    stopped_check: Returned check (stopped)

prompts:
  client_name: >-
    What is your full legal name? (The statement of claim prints it with
    your contact information in the claimant section.)
  people__claimants: >-
    Your mailing address — street, borough or city, state, and ZIP. (No
    P.O. boxes; the court uses this address for hearing notices.)
  person__defendant: >-
    Who are you suing? Type the defendant's full name — first name,
    middle initial, and last name as you know them.
  people__defendants: >-
    The defendant's mailing address — street, borough or city, and ZIP.
    (The court serves the claim at this address.)
  custom_phone__defendant_contact: >-
    The defendant's telephone number, if you know it. (Leave blank if you
    do not.)
  claim_amount: >-
    How much are you claiming? (A dollar amount — the small-claims limit
    in the New York City Civil Court is $10,000.)
  primary_claim_reason: >-
    What is the primary reason for your claim? (The form asks you to
    check one — pick the closest reason; you can explain the details in
    the next questions.)
  claim_occurrence_date: >-
    When did the occurrence or transaction happen? (An approximate date
    is fine when you are not sure of the exact day.)
  auto_accident_place: >-
    If your claim is about an automobile accident, where did it happen?
    (Leave blank for any other claim type.)
  claim_other_description: >-
    Briefly describe what happened if the reason list did not cover it.
    (Be brief — the form gives this explanation one line.)
  identifying_numbers: >-
    Any identifying numbers for the claim — contract, invoice, or account
    numbers. (Leave blank if there are none.)

workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    claim_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__small_claims_statement_of_claim_pdf
    rejected: END
  generate_pdf__small_claims_statement_of_claim_pdf:
    pdf_persisted: filing__nyc_civil_court
  filing__nyc_civil_court:
    filed: END
  END: {}
---

This New York engagement files the New York City Civil Court's own Statement of Claim (CIV-SC-50)
for `{{person__client.name}}` against the defendant named in the intake, in the Small Claims Part.

Small claims in the New York City Civil Court are for individuals only — corporations and other
business entities cannot sue in this part (though they can be sued). The claim limit is $10,000.
The statement of claim names one defendant per filing; a claim against more than one defendant
files once per defendant, and the intake prepares one blank at a time.

The filing fee is $15.00 for claims up to $1,000 and $20.00 for larger claims. A party who cannot
afford the fee applies on the court's own fee-waiver form, or by poor-person motion; the fee-note
in the registry points at both. Claims cannot be filed electronically — the firm files the paper
statement with the Small Claims Part clerk.

The intake takes eight core questions — who you are, who you are suing, how much you are claiming,
and the primary reason for the claim. The form's own printed reason grid offers twenty-eight
choices, which the intake carries as a single choice question rather than twenty-eight boxes.
Three optional detail questions follow — the place of an automobile accident, a one-line
explanation when the reason list did not cover the claim, and any identifying numbers — and are
skipped when blank.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney
reviews the **filled statement of claim** before anything is filed. Nothing reaches the courthouse
unreviewed. You sign at the review appointment, and the firm files the statement with the Small
Claims Part clerk; the clerk schedules the hearing and completes the court's own sections.
