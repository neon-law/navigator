---
kind: filing
title: Civil Case Cover Sheet
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/CM010.pdf
confidential: true
output: form
form: ca__civil_case_cover_sheet
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
    cover_sheet_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__civil_case_cover_sheet_pdf
    rejected: END
  generate_pdf__civil_case_cover_sheet_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This Civil Case Cover Sheet (form CM-010) is filed with the first paper in
unlimited civil cases, including a petition for change of name, so the clerk
can route the case to the right courtroom and case category. It carries the
Petitioner's name, the new name requested, the county of filing, and the
case-type cross-reference.

Which courts require CM-010 varies; several (including Los Angeles and San
Francisco) do for name-change petitions. The case-type checkboxes on the
official form are checked by the firm for the specific county's practice, and
the case number and court-use boxes are completed by the clerk — those stay
for the filing appointment.

Your answers above are placed onto the court's own official blank and a
licensed Neon Law attorney reviews the **filled cover sheet** before anything
is filed. Nothing reaches the courthouse unreviewed.
