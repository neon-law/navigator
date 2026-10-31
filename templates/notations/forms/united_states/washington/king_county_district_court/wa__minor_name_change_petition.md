---
kind: filing
title: Petition for Minor Child Name Change
respondent_type: person
jurisdiction: WA
origin_url: https://cdn.kingcounty.gov/-/media/king-county/courts/district-court/forms_resources_library_docs/name-change-forms/petition-for-name-change-minor-october-2024.pdf
confidential: true
output: form
form: wa__minor_name_change_petition
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: people__parents
  people__parents:
    _: custom_datetime__date_of_birth
  custom_datetime__date_of_birth:
    _: custom_yes_no__sex_offender_registration
  custom_yes_no__sex_offender_registration:
    _: custom_yes_no__prior_name_change
  custom_yes_no__prior_name_change:
    _: custom_text__prior_name_change_details
  custom_text__prior_name_change_details:
    _: custom_text__best_interest_reasons
  custom_text__best_interest_reasons:
    _: END
  END: {}
prompts:
  client_name: >-
    What is the child's current full legal name, as it appears on the child's
    identification?
  parents: >-
    Who are the child's parents or legal guardians? (Both, if living — the court
    needs each parent's consent or proof the other parent was served.)
  date_of_birth: What is the child's date of birth?
  sex_offender_registration: Is the child required to register as a sex offender?
  prior_name_change: Has the child had a name change before this petition?
  prior_name_change_details: >-
    If yes, briefly describe the earlier change — the date, place, court, and case
    number if you know them. Leave blank if there was none.
  best_interest_reasons: >-
    Briefly, why is this change in the child's best interest? (The court accepts
    any honest reason; the child joins the petition if they are 14 or older.)
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__minor_name_change_petition_pdf
    rejected: END
  generate_pdf__minor_name_change_petition_pdf:
    pdf_persisted: filing__king_county_district_court
  filing__king_county_district_court:
    filed: END
  END: {}
---

This King County engagement petitions the King County District Court, on the county's own local
form, to change the legal name of the minor child `{{person__client.name}}` under RCW 4.24.130, on
the petition of the child's parents or lawful guardians.

The petition states the child's date of birth and sex-offender-registration disclosure, any prior
name change with its date, place, court, and case number, and the parents' own statement of why the
change is in the child's best interest. Both parents' consent — or service on the parent who has not
signed — is required, and the other parent's status is confirmed with the attorney at the review
appointment. A child who is 14 or older joins in the petition and must attend the hearing.

The filing fee is $98 per application (one certified copy included; additional copies $5), and the
King County Recorder charges a separate recording fee ($203.50 individual; $204.50 for the first
family member plus $203.50 for each additional). A petitioner who cannot afford either fee moves to
waive it on the King County local GR 34 motion or the statewide GR 34 set. A sealed minor name
change (gender expression or identity, refugee or asylum status, a domestic-violence victim, an
emancipated minor) belongs in Superior Court instead.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney
reviews the **filled petition** before anything is filed. Nothing reaches the courthouse unreviewed.
The petitioning parents sign the petition at the review appointment, and the firm files it with the
district court clerk (the court accepts eFiling through dcfiling.kingcounty.gov).
