---
kind: filing
title: Small Claims Counterclaim (Las Vegas)
respondent_type: person
jurisdiction: NV
origin_url: https://www.civillawselfhelpcenter.org/forms/small-claims-forms/284-las-vegas-small-claims-forms
confidential: true
output: form
form: nv__las_vegas_small_claims_counterclaim
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_text__counterplaintiff_caption
  custom_text__counterplaintiff_caption:
    _: custom_text__counterdefendant_caption
  custom_text__counterdefendant_caption:
    _: custom_usd__amount_owed
  custom_usd__amount_owed:
    _: custom_text__reason_owed
  custom_text__reason_owed:
    _: custom_text__reason_owed_2
  custom_text__reason_owed_2:
    _: custom_text__reason_owed_3
  custom_text__reason_owed_3:
    _: custom_text__reason_owed_4
  custom_text__reason_owed_4:
    _: custom_text__reason_owed_5
  custom_text__reason_owed_5:
    _: custom_text__reason_owed_6
  custom_text__reason_owed_6:
    _: END
  END: {}
prompts:
  client_name: >-
    What is your full legal name? (You are the counterplaintiff — the original defendant answering with your own claim.)
  counterplaintiff_caption: Your full name and mailing address, as the caption should read.
  counterdefendant_caption: >-
    The counterdefendant's full name and mailing address — the original plaintiff you have a claim against.
  amount_owed: >-
    How much does the counterdefendant owe you? (A dollar amount — the justice-court small-claims limit is $10,000.)
  reason_owed: >-
    Briefly, why does the counterdefendant owe you this money? (The sworn statement tells the court what happened.)
  reason_owed_2: If you need more room, continue your reason here. (Leave blank if not.)
  reason_owed_3: Continue your reason here if you need more room. (Leave blank if not.)
  reason_owed_4: Continue your reason here if you need more room. (Leave blank if not.)
  reason_owed_5: Continue your reason here if you need more room. (Leave blank if not.)
  reason_owed_6: Finish your reason here if you need one more line. (Leave blank if not.)
workflow:
  BEGIN:
    intake_submitted: intake_persisted__client
  intake_persisted__client:
    petition_rendered: lawyer_review
  lawyer_review:
    approved: generate_pdf__small_claims_counterclaim_pdf
    rejected: END
  generate_pdf__small_claims_counterclaim_pdf:
    pdf_persisted: filing__las_vegas_justice_court
  filing__las_vegas_justice_court:
    filed: END
  END: {}
---

This Nevada engagement files the Las Vegas Justice Court's own small-claims counterclaim for `{{person__client.name}}` —
the original defendant — against the original plaintiff, to recover `{{custom_usd__amount_owed}}`.

The counterclaim is a sworn statement filed in the same case: it names the counterplaintiff and counterdefendant with
their addresses, states the sum owed and the reason for the indebtedness across the form's six lines, and declares that
payment was demanded and refused. The venue paragraph on the form identifies the Las Vegas Township, Clark County, as
proper under NRS 73.010.

There is no separate filing fee recorded for the counterclaim; the clerk confirms the current amount when the case is
filed. A party who cannot afford any fee applies to waive it on the justice-court fee-waiver application.

Your answers above are placed onto the court's own official blank and a licensed Neon Law attorney reviews the **filled
counterclaim** before anything is filed. Nothing reaches the courthouse unreviewed. You sign the sworn counterclaim at
the review appointment, and the firm files it with the small-claims division of the Las Vegas Justice Court clerk. The
trial date, time, and location on the order to appear are completed by the clerk after filing.
