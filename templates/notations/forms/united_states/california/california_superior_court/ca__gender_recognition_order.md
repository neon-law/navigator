---
kind: filing
title: >-
  Order Recognizing Adult's Change of Gender and Sex Identifier, for Name Change and for Issuance of New Certificates
respondent_type: person
jurisdiction: CA
origin_url: https://www.courts.ca.gov/documents/NC330.pdf
confidential: true
output: form
form: ca__gender_recognition_order
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_single_choice__sex_identifier
  custom_single_choice__sex_identifier:
    _: person__new_name
  person__new_name:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: custom_single_choice__new_certificates
  custom_single_choice__new_certificates:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  sex_identifier: What sex identifier are you asking the court to recognize for you?
  new_name: What new name are you asking the court for, if any?
  date_of_birth: What is your date of birth?
  county_of_residence: In what California county do you live?
  new_certificates: Which new certificates should the court order issued to match your change?
choices:
  sex_identifier:
    female: Female
    male: Male
    nonbinary: Nonbinary
  new_certificates:
    birth_certificate: Birth certificate
    childs_birth_certificate: Child's birth certificate
    marriage_license: Marriage license and certificate
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    order_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__gender_recognition_order_pdf
    rejected: END
  generate_pdf__gender_recognition_order_pdf:
    pdf_persisted: filing__ca_superior_court
  filing__ca_superior_court:
    filed: END
  END: {}
---

This order (form NC-330) is the judgment the Superior Court of `{{custom_text__county_of_residence}}` County signs after
the hearing on the NC-300 petition, recognizing that the gender and sex identifier of `{{person__client.name}}` (the
"Petitioner") is `{{custom_single_choice__sex_identifier}}`, ordering any requested name change, and directing the
issuance of new certificates. The certified order is what vital-records offices and the DMV require.

The court completes the findings, the certificate specifics, and the judge's signature; the firm prepares the order with
the Petitioner's information so the judge's version needs only findings and a signature.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
order** before the hearing. Nothing reaches the courthouse unreviewed.
