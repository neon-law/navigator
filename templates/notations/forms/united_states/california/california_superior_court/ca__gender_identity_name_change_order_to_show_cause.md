---
kind: filing
title: Order to Show Cause—Change of Name to Conform to Gender Identity
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC225.pdf
confidential: true
output: form
form: ca__gender_identity_name_change_order_to_show_cause
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
    order_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__gender_identity_osc_pdf
    rejected: END
  generate_pdf__gender_identity_osc_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This Order to Show Cause (form NC-225) is the paper the Superior Court of `{{custom_text__county_of_residence}}` County
signs to set the hearing on the NC-200 petition of `{{person__client.name}}` (the "Petitioner") to change the
Petitioner's name to `{{person__new_name.name}}` to conform to gender identity and to recognize the Petitioner's sex
identifier. Because the change conforms to gender identity, no newspaper publication is ordered.

The hearing date and the judge's signature block are completed by the court; the firm prepares everything up to the
clerk's stamp.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
order** before anything is filed. Nothing reaches the courthouse unreviewed.
