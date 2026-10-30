---
kind: filing
title: Order to Show Cause—Change of Name
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC120.pdf
confidential: true
output: form
form: ca__name_change_order_to_show_cause
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_text__name_changing_to
  custom_text__name_changing_to:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  name_changing_to: What new name are you asking the court for?
  date_of_birth: What is your date of birth?
  county_of_residence: In what California county do you live?
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    order_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__name_change_order_to_show_cause_pdf
    rejected: END
  generate_pdf__name_change_order_to_show_cause_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This Order to Show Cause (form NC-120) is the paper the Superior Court of
`{{custom_text__county_of_residence}}` County signs to set the hearing on the
name-change petition of `{{person__client.name}}` (the "Petitioner") and to
order that the petition be published in a newspaper of general circulation
unless publication is excused. It identifies the Petitioner's present name, the
new name requested (`{{custom_text__name_changing_to}}`), and the hearing date
the clerk assigns.

The hearing date itself, the judge's signature block, and the proof of
publication are completed by the court and the newspaper; the firm prepares
everything up to the clerk's stamp.

Your answers above are placed onto the court's own official blank and a
licensed Neon Law attorney reviews the **filled order** before anything is
filed. Nothing reaches the courthouse unreviewed.
