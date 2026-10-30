---
kind: filing
title: Name and Information About the Person Whose Name is to be Changed
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC110.pdf
confidential: true
output: form
form: ca__name_change_attachment
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__new_name
  person__new_name:
    _: custom_single_choice__current_sex_designation
  custom_single_choice__current_sex_designation:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__place_of_birth
  custom_text__place_of_birth:
    _: custom_text__reason_for_change
  custom_text__reason_for_change:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  new_name: What new name are you asking the court for? Spell it exactly as you want it to read on the decree.
  current_sex_designation: What is your current sex designation, as the attachment prints it?
  date_of_birth: What is your date of birth?
  place_of_birth: Where were you born (city and state)?
  reason_for_change: Briefly, why are you asking for the change?
choices:
  current_sex_designation:
    female: Female
    male: Male
    nonbinary: Nonbinary
    prefer_not_to_say: Prefer not to state
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    attachment_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__name_change_attachment_pdf
    rejected: END
  generate_pdf__name_change_attachment_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This attachment to the Petition for Change of Name (form NC-110) gives the Superior Court the identifying information
about `{{person__client.name}}` (the "Petitioner") that the NC-100 petition itself does not carry: the present legal
name, the name the Petitioner is asking for (`{{person__new_name.name}}`), the Petitioner's date of birth, residence,
and current sex designation.

It is filed together with the NC-100 petition, the NC-120 Order to Show Cause, and — after the hearing — the NC-130
Decree Changing Name. The same answers fill all four forms; you answer the questions once.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
attachment** before anything is filed. The declarations at the bottom of the attachment are confirmed with the
Petitioner at the review appointment and signed there.
