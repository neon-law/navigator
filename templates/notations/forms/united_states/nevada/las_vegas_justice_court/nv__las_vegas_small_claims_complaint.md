---
kind: filing
title: Small Claims Complaint (Las Vegas)
respondent_type: person
jurisdiction: NV
origin_url: https://www.civillawselfhelpcenter.org/images/small-claims/complaints/complaint---small-claims.pdf
confidential: true
output: form
form: nv__las_vegas_small_claims_complaint
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_text__plaintiff_caption
  custom_text__plaintiff_caption:
    _: custom_text__defendant_caption
  custom_text__defendant_caption:
    _: custom_phone__defendant_contact
  custom_phone__defendant_contact:
    _: custom_usd__amount_owed
  custom_usd__amount_owed:
    _: custom_text__reason_owed
  custom_text__reason_owed:
    _: custom_single_choice__venue_township
  custom_single_choice__venue_township:
    _: END
  END: {}
choices:
  venue_township:
    Las Vegas: Las Vegas
    Henderson: Henderson
    North Las Vegas: North Las Vegas
    Boulder: Boulder
    Bunkerville: Bunkerville
    Goodsprings: Goodsprings
    Laughlin: Laughlin
    Mesquite: Mesquite
    Moapa: Moapa
    Moapa Valley: Moapa Valley
    Searchlight: Searchlight

prompts:
  client_name: >-
    What is your full legal name? (It appears on the sworn statement and
    beside your signature.)
  plaintiff_caption: >-
    Your full name and mailing address, as the caption should read. (The court
    mails hearing notices to this address.)
  defendant_caption: >-
    The defendant's full name and mailing address. (The court serves the
    complaint at this address.)
  custom_phone__defendant_contact: >-
    The defendant's telephone number, if you know it. (Leave blank if you
    do not — the complaint carries one telephone box for the defendant.)
  amount_owed: >-
    How much does the defendant owe you? (A dollar amount — the claim limit in
    justice-court small claims is $10,000.)
  reason_owed: >-
    Briefly, why does the defendant owe you this money? (The complaint's sworn
    statement tells the court what happened.)
  venue_township: >-
    In which township will the case be filed? (Where the defendant lives or
    the debt arose — the attorney confirms the venue with you.)
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__small_claims_complaint_pdf
    rejected: END
  generate_pdf__small_claims_complaint_pdf:
    pdf_persisted: filing__las_vegas_justice_court
  filing__las_vegas_justice_court:
    filed: END
  END: {}
---

This Nevada engagement files the Las Vegas Justice Court's own small-claims complaint for
`{{person__client.name}}` against the defendant named in the caption, to recover
`{{custom_usd__amount_owed}}` — the justice-court small-claims limit is $10,000.

The complaint is a sworn statement: it names the plaintiff and defendant with their addresses, states
the sum owed and the reason for the indebtedness, and declares that payment was demanded and refused.
The venue township — where the defendant lives or the debt arose under NRS 73.010 — is selected from
the court's own dropdown and confirmed with the attorney before filing.

The filing fee is tiered by claim under the NRS 4.060 schedule: $66 up to $1,000; $86 for $1,000.01
to $2,500; $106 for $2,500.01 to $5,000; $146 for $5,000.01 to $7,500; $196 for $7,500.01 to $10,000.
A party who cannot afford the fee applies to waive it on the justice-court fee-waiver application.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney
reviews the **filled complaint** before anything is filed. Nothing reaches the courthouse unreviewed.
You sign the sworn complaint at the review appointment, and the firm files it with the small-claims
division of the Las Vegas Justice Court clerk. The trial date, time, and location on the order to
appear are completed by the clerk after filing.
