---
kind: filing
title: Petition for Adult Name Change (Washoe)
respondent_type: person
jurisdiction: NV
origin_url: https://www.washoecourts.com/Forms/Family/10.%20Name%20Changes/NA%20Adult%20Name%20Change/1.%20Petition%20for%20Adult%20Name%20Change.pdf
confidential: true
output: form
form: nv__washoe_adult_name_change_petition
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
    _: custom_datetime__county_residency_since
  custom_datetime__county_residency_since:
    _: custom_text__reason_for_change
  custom_text__reason_for_change:
    _: custom_yes_no__felony_conviction
  custom_yes_no__felony_conviction:
    _: custom_yes_no__criminal_record
  custom_yes_no__criminal_record:
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
  county_residency_since: When did you move to Washoe County? The petition states you have lived there since that date.
  reason_for_change: >-
    Briefly, why are you asking for the change? (The petition tells the court the change is not for a fraudulent or
    illegal purpose.)
  felony_conviction: Have you ever been convicted of a felony?
  criminal_record: Do you have any other criminal record?
  felony_details: >-
    If you answered yes to either question, briefly describe each conviction or record — the offense, the date, the
    county and state, and the final result. Leave blank if none.
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__adult_name_change_petition_pdf
    rejected: END
  generate_pdf__adult_name_change_petition_pdf:
    pdf_persisted: filing__second_judicial_district_court
  filing__second_judicial_district_court:
    filed: END
  END: {}
---

This Nevada engagement petitions the Second Judicial District Court in and for Washoe County on the court's own NA
packet form to change the legal name of `{{person__client.name}}` (the "Petitioner") to `{{person__new_name.name}}`.

The petition states the Petitioner's date of birth and birthplace, that the Petitioner has lived in Washoe County since
`{{custom_datetime__county_residency_since}}`, the reason for the requested change, and — as the statute requires — that
the change is not sought for any fraudulent or illegal purpose, together with the felony and criminal-record
disclosures. The petition also carries the gender-identity and other reason categories; the attorney selects those
categories with the Petitioner at review, because checking the gender-identity box asserts the statutory exemption from
publication.

The NA packet's filing fee is not published on the court's site; the firm confirms the current amount with the clerk and
can apply to waive fees on the court's F-6 application. If the Petitioner has a criminal record, a complete set of
original fingerprints must be lodged in the filing office before the hearing.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
petition** before anything is filed. Nothing reaches the courthouse unreviewed. The Petitioner signs the petition and
its verification before a notary or the clerk at the review appointment, and the firm files it with the Family Division
clerk.
