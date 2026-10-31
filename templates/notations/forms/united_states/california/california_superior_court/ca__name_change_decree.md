---
kind: filing
title: Decree Changing Name
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC130.pdf
confidential: true
output: form
form: ca__name_change_decree
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__new_name
  person__new_name:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  new_name: What new name are you asking the court for?
  date_of_birth: What is your date of birth?
  county_of_residence: In what California county do you live?
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    decree_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__name_change_decree_pdf
    rejected: END
  generate_pdf__name_change_decree_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This Decree Changing Name (form NC-130) is the order the Superior Court of `{{custom_text__county_of_residence}}` County
signs after the hearing, once the publication requirement is satisfied, changing the legal name of
`{{person__client.name}}` (the "Petitioner") to `{{person__new_name.name}}`. The certified decree is the document the
Petitioner takes to the DMV, the SSA, banks, and vital-records offices.

The court completes the decree at or after the hearing; the firm prepares it with the Petitioner's information so the
judge's version needs only findings and a signature.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
decree** before the hearing. Nothing reaches the courthouse unreviewed.
