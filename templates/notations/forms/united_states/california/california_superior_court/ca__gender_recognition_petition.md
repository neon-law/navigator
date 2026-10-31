---
kind: filing
title: >-
  Petition for Recognition of Adult's Change of Gender and Sex Identifier, Name Change, and Issuance of New Certificates
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC300.pdf
confidential: true
output: form
form: ca__gender_recognition_petition
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_single_choice__sex_identifier
  custom_single_choice__sex_identifier:
    _: custom_single_choice__new_certificates
  custom_single_choice__new_certificates:
    _: person__new_name
  person__new_name:
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
  sex_identifier: What sex identifier are you asking the court to recognize for you?
  new_certificates: Which new certificates should the court order issued for you?
  new_name: >-
    What new name are you asking the court for, if any? Leave blank if you are not asking for a name change on this
    petition.
  current_sex_designation: What is your current sex designation on existing documents?
  date_of_birth: What is your date of birth?
  place_of_birth: Where were you born (city and state)?
  county_of_residence: >-
    In what California county do you live? The petition is filed in the Superior Court of your county.
choices:
  sex_identifier:
    female: Female
    male: Male
    nonbinary: Nonbinary
  new_certificates:
    none: No new certificates
    birth_certificate: Birth certificate
    childs_birth_certificate: A child's birth certificate
    marriage_license: Marriage license and certificate
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
    approved: generate_pdf__gender_recognition_petition_pdf
    rejected: END
  generate_pdf__gender_recognition_petition_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This California engagement asks the Superior Court of `{{custom_text__county_of_residence}}` County to recognize the
change of gender and sex identifier of `{{person__client.name}}` (the "Petitioner") to
`{{custom_single_choice__sex_identifier}}` and to order new certificates issued, on Judicial Council form NC-300. Where
the Petitioner also asks for a name change on this petition, the petition carries it — no separate publication applies.

California birth certificates and driver's licenses can be updated through the Department of Public Health and the DMV
without a court order; a court order like this one is needed for out-of-state birth certificates and for a name change.
A court decision follows a six-week waiting period from filing.

Your answers above are placed onto the court's own official blank — the same form the Judicial Council publishes — and a
licensed Neon Law attorney reviews the **filled petition** before anything is filed. Nothing reaches the courthouse
unreviewed. The Petitioner verifies and signs the petition at the review appointment, and the firm files it with the
clerk of the Superior Court.
