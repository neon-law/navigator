---
kind: filing
title: Decree Changing Name to Conform to Gender Identity
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC230.pdf
confidential: true
output: form
form: ca__gender_identity_name_change_decree
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__new_name
  person__new_name:
    _: custom_single_choice__sex_identifier
  custom_single_choice__sex_identifier:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  new_name: What new name are you asking the court for?
  sex_identifier: What sex identifier are you asking the court to recognize for you?
  date_of_birth: What is your date of birth?
  county_of_residence: In what California county do you live?
choices:
  sex_identifier:
    female: Female
    male: Male
    nonbinary: Nonbinary
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    decree_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__gender_identity_decree_pdf
    rejected: END
  generate_pdf__gender_identity_decree_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This Decree Changing Name (form NC-230, the 200-series decree) is the order the Superior Court of
`{{custom_text__county_of_residence}}` County signs after the hearing, changing the legal name of
`{{person__client.name}}` (the "Petitioner") to `{{person__new_name.name}}` to conform to the Petitioner's gender
identity. The certified decree is the document the Petitioner takes to the DMV, the SSA, banks, and vital-records
offices. Like the NC-200 petition it follows, no newspaper publication applies.

The court completes the decree at or after the hearing; the firm prepares it with the Petitioner's information so the
judge's version needs only findings and a signature.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
decree** before the hearing. Nothing reaches the courthouse unreviewed.
