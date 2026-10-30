---
kind: filing
title: Petition for Change of Name to Conform to Gender Identity
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC200.pdf
confidential: true
output: form
form: ca__gender_identity_name_change_petition
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_text__name_changing_to
  custom_text__name_changing_to:
    _: custom_single_choice__sex_identifier
  custom_single_choice__sex_identifier:
    _: custom_single_choice__current_sex_designation
  custom_single_choice__current_sex_designation:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__place_of_birth
  custom_text__place_of_birth:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  name_changing_to: What new name are you asking the court for, if any? Leave blank if you are only asking the court to recognize your gender.
  sex_identifier: What sex identifier are you asking the court to recognize for you?
  current_sex_designation: What is your current sex designation on existing documents?
  date_of_birth: What is your date of birth?
  place_of_birth: Where were you born (city and state)?
  county_of_residence: In what California county do you live? The petition is filed in the Superior Court of your county.
choices:
  sex_identifier:
    female: Female
    male: Male
    nonbinary: Nonbinary
  current_sex_designation:
    female: Female
    male: Male
    nonbinary: Nonbinary
    prefer_not_to_say: Prefer not to state
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__gender_identity_petition_pdf
    rejected: END
  generate_pdf__gender_identity_petition_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This California engagement asks the Superior Court of
`{{custom_text__county_of_residence}}` County to change the legal name of
`{{person__client.name}}` (the "Petitioner") to
`{{custom_text__name_changing_to}}` to conform to the Petitioner's gender
identity, on Judicial Council form NC-200.

A name change to conform to gender identity needs **no newspaper publication**,
so the NC-120 publication requirement that applies to other name changes does
not apply here. The court decides these petitions in about one to two months;
no medical documentation is required.

Your answers above are placed onto the court's own official blank — the same
form the Judicial Council publishes — and a licensed Neon Law attorney reviews
the **filled petition** before anything is filed. Nothing reaches the courthouse
unreviewed. The Petitioner verifies and signs the petition at the review
appointment, and the firm files it with the clerk of the Superior Court.
