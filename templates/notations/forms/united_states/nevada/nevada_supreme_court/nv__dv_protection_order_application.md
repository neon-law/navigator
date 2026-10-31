---
kind: filing
title: Application for Protection Order Against Domestic Violence
respondent_type: person
jurisdiction: NV
origin_url: https://selfhelp.nvcourts.gov/images/protection-orders/dv-app-pdf-fillable.pdf
confidential: true
output: form
form: nv__dv_protection_order_application
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__respondent
  person__respondent:
    _: custom_single_choice__relationship
  custom_single_choice__relationship:
    _: custom_yes_no__living_together
  custom_yes_no__living_together:
    _: custom_single_choice__who_was_harmed
  custom_single_choice__who_was_harmed:
    _: custom_text__incident_date
  custom_text__incident_date:
    _: custom_text__incident_location
  custom_text__incident_location:
    _: custom_text__what_happened
  custom_text__what_happened:
    _: custom_yes_no__weapon_involved
  custom_yes_no__weapon_involved:
    _: custom_yes_no__incident_reported
  custom_yes_no__incident_reported:
    _: custom_yes_no__emergency_residence_stay_away
  custom_yes_no__emergency_residence_stay_away:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: END
  END: {}
prompts:
  client_name: Confirm your name as the applicant for protection.
  respondent_name: Who do you want protection from? (This person is the adverse party.)
  relationship: What is your relationship to the adverse party?
  living_together: Do you and the adverse party live together?
  who_was_harmed: Who did the domestic violence happen to?
  incident_date: When did the most recent incident happen? (A date, or "ongoing".)
  incident_location: Where did it happen?
  what_happened: In your own words, what happened during the most recent incident?
  weapon_involved: Did the adverse party use or threaten to use a weapon?
  incident_reported: Was the incident reported to the police?
  emergency_residence_stay_away: >-
    Should the adverse party be ordered to stay at least 100 yards away from your current residence?
  county_of_residence: Which Nevada county is the application being filed in?
choices:
  relationship:
    married: Married or formerly married
    dating: Dating or formerly dated
    coparents: We have a child in common
    household: Live or used to live together / related by blood or marriage
    other: Other qualifying relationship
  who_was_harmed:
    applicant: Me (the applicant)
    child_parent: A minor child — I am the child's parent
    child_guardian: A minor child — I am the child's legal guardian
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    application_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__dv_protection_order_application_pdf
    rejected: END
  generate_pdf__dv_protection_order_application_pdf:
    pdf_persisted: filing__nv_protection_order
  filing__nv_protection_order:
    filed: END
  END: {}
---

This Nevada engagement applies, on the Nevada Supreme Court self-help form approved for use in any Nevada court, for a
protection order against domestic violence for `{{person__client.name}}` (the "Applicant") against
`{{person__respondent.name}}` (the "Adverse Party"). The application describes the most recent incident
(`{{custom_text__incident_date}}`, `{{custom_text__incident_location}}`: `{{custom_text__what_happened}}`), the parties'
relationship, any weapons, any police report, and the emergency protections the Applicant asks the judge to order
same-day. There is no fee to file.

A temporary order can issue within one business day without notice to the adverse party; an extended order (up to two
years) follows a hearing. The application's detailed relief selections — workplace and school addresses, custody and
visitation of the children, personal-belongings retrieval, firearms surrender terms — are confirmed with the Applicant
at the review appointment and completed by the firm, because they become binding order terms.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
application** before anything is filed. Nothing reaches the courthouse unreviewed. The firm submits the application to
the Protection Order Help Center or the clerk of the court, and returns the judge's decision to the Applicant.
