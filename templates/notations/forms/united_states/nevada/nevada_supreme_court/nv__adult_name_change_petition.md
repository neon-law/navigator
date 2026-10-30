---
kind: filing
title: Petition for Adult Name Change
respondent_type: person
jurisdiction: NV
origin_url: https://selfhelp.nvcourts.gov/images/name-change/name-adult-petition-pdf-fillable.pdf
confidential: true
output: form
form: nv__adult_name_change_petition
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__new_name
  person__new_name:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_text__birthplace_city
  custom_text__birthplace_city:
    _: jurisdiction__birthplace
  jurisdiction__birthplace:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: custom_text__reason_for_change
  custom_text__reason_for_change:
    _: custom_yes_no__felony_conviction
  custom_yes_no__felony_conviction:
    _: custom_text__felony_details
  custom_text__felony_details:
    _: END
  END: {}
prompts:
  client_name: What is your current full legal name, as it appears on your identification?
  new_name: >-
    What will your new full legal name be? (First, middle, and last — leave the middle name out if there is none.)
  date_of_birth: What is your date of birth?
  birthplace_city: In what city were you born?
  birthplace: In what state or country were you born?
  county_of_residence: In what Nevada county do you live? The petition is filed in the District Court of your county.
  reason_for_change: >-
    Briefly, why are you asking for the change? (The petition tells the court the change is not for a fraudulent or
    illegal purpose.)
  felony_conviction: Have you ever been convicted of a felony?
  felony_details: If yes, briefly describe the conviction. Leave blank if none.
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__adult_name_change_petition_pdf
    rejected: END
  generate_pdf__adult_name_change_petition_pdf:
    pdf_persisted: filing__nv_district_court
  filing__nv_district_court:
    filed: END
  END: {}
---

This Nevada engagement petitions the District Court of `{{custom_text__county_of_residence}}` County, on the Nevada
Supreme Court self-help form approved for use in any Nevada court, to change the legal name of `{{person__client.name}}`
(the "Petitioner") to `{{person__new_name.name}}`.

The petition states the Petitioner's date of birth and birthplace, the county where the Petitioner lives, the reason for
the requested change, and — as the statute requires — that the change is not sought for any fraudulent or illegal
purpose, together with the felony-disclosure answer. The filing fee is set by each county; the firm confirms the amount
for the Petitioner's county and can apply for a waiver of the fee on the statewide application.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
petition** before anything is filed. Nothing reaches the courthouse unreviewed. The Petitioner signs the petition before
a notary or the clerk at the review appointment, and the firm files it with the District Court clerk.
