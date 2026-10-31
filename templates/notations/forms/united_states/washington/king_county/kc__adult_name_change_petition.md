---
kind: filing
title: Petition for Name Change (Individual)
respondent_type: person
jurisdiction: WA
origin_url: https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-individual-july_2026.pdf
confidential: true
output: form
form: kc__adult_name_change_petition
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__new_name
  person__new_name:
    _: custom_yes_no__sex_offender_registration
  custom_yes_no__sex_offender_registration:
    _: custom_yes_no__doc_jurisdiction
  custom_yes_no__doc_jurisdiction:
    _: custom_yes_no__prior_name_change
  custom_yes_no__prior_name_change:
    _: custom_text__reason_for_change
  custom_text__reason_for_change:
    _: custom_yes_no__not_detrimental
  custom_yes_no__not_detrimental:
    _: custom_yes_no__open_protection_order
  custom_yes_no__open_protection_order:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  new_name: >-
    What new name are you asking the court for? Spell it exactly as you want it to read on the decree
    (first, middle, and last — leave the middle name out if there is none).
  sex_offender_registration: >-
    Are you required to register as a sex offender? (If yes, the petitioner must notify the King County
    Sheriff and Washington State Patrol of this change; failing to do so is a crime.)
  doc_jurisdiction: >-
    Are you under the jurisdiction of the Department of Corrections? (If yes, DOC must be notified of
    this change; failing to do so is a crime.)
  prior_name_change: Have you had a name change before this petition?
  reason_for_change: Briefly, why are you asking for the change? (The court accepts any honest reason.)
  not_detrimental: >-
    Do you state that the name change is not detrimental to the interests of anyone else?
  open_protection_order: >-
    Are you the defendant or respondent to any open or pending protection order?
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__adult_name_change_petition_pdf
    rejected: END
  generate_pdf__adult_name_change_petition_pdf:
    pdf_persisted: filing__king_county_district_court
  filing__king_county_district_court:
    filed: END
  END: {}
---

This King County engagement petitions the King County District Court, on the county's own local
form, to change the legal name of `{{person__client.name}}` (the "Petitioner") to
`{{person__new_name.name}}` under RCW 4.24.130.

The petition states the Petitioner's sex-offender-registration, Department of Corrections, and
prior-name-change disclosures, the Petitioner's own reason for the change, and the statements that
the change is not detrimental to anyone else's interests and that the Petitioner is not the
respondent to any open protection order. The district court reports every name change to the King
County Recorder and does not automatically seal its files; a petitioner who needs a sealed record
files in Superior Court instead.

The filing fee is $98 per application (one certified copy included; additional copies $5), and the
King County Recorder charges a separate recording fee ($203.50 individual; $204.50 for the first
family member plus $203.50 for each additional). A petitioner who cannot afford either fee moves to
waive it on the King County local GR 34 motion or the statewide GR 34 set.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney
reviews the **filled petition** before anything is filed. Nothing reaches the courthouse unreviewed.
The Petitioner signs the petition at the review appointment, and the firm files it with the district
court clerk (the court accepts eFiling through dcfiling.kingcounty.gov).
