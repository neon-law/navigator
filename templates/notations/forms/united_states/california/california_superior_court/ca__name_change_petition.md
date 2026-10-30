---
kind: filing
title: Petition for Change of Name
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC100.pdf
confidential: true
output: form
form: ca__name_change_petition
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_text__name_changing_to
  custom_text__name_changing_to:
    _: custom_single_choice__current_sex_designation
  custom_single_choice__current_sex_designation:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  name_changing_to: What new name are you asking the court for? Spell it exactly as you want it to read on the decree.
  current_sex_designation: What is your current sex designation, as the petition prints it?
  date_of_birth: What is your date of birth?
  county_of_residence: In what California county do you live? The petition is filed in the Superior Court of your county.
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
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__name_change_petition_pdf
    rejected: END
  generate_pdf__name_change_petition_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This California name-change engagement (the "Engagement") asks the Superior
Court of `{{custom_text__county_of_residence}}` County to change the legal name
of `{{person__client.name}}` (the "Petitioner") to
`{{custom_text__name_changing_to}}`, on Judicial Council form NC-100.

The Petition asks the court to order that the Petitioner's name be changed,
that the Order to Show Cause (NC-120) issue, and that a certified Decree
Changing Name (NC-130) be issued after the hearing. Because a California name
change is generally published in a newspaper of general circulation for one
month, the Engagement includes preparing the publication order; publication is
not required where the petition also asks the court to conform the name to
gender identity, which is handled on the NC-200 petition instead.

Your answers above are placed onto the court's own official blank — the same
form the Judicial Council publishes — and a licensed Neon Law attorney reviews
the **filled petition** before anything is filed. Nothing reaches the courthouse
unreviewed. The Petitioner verifies and signs the petition at the review
appointment, and the firm files it with the clerk of the Superior Court and
returns the filed copy.
