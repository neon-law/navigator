---
kind: pleading
title: Synthetic Residential Schedule (Washington)
caption_title: PROPOSED RESIDENTIAL SCHEDULE
footer_title: PROPOSED RESIDENTIAL SCHEDULE
docket: 26-2-00000-SAMPLE
jurisdiction: WA
respondent_type: person
confidential: false
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_datetime__verification_date
  custom_datetime__verification_date:
    _: END
  END: {}
prompts:
  verification_date: On what date does the client sign the verification?
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    _: END
  END: {}
---

# PROPOSED RESIDENTIAL SCHEDULE

## 1. Residential Schedule

1. The child resides with the custodial parent.
2. The other parent has visitation every other weekend.
3. Holidays alternate between the parents each year.

{{person__client}} ({{custom_datetime__verification_date}}).

## 2. Selection

☐ Proposed by mother    ☐ Proposed by father
