---
kind: pleading
title: Synthetic Notice of Compliance (California)
caption_title: PLAINTIFF'S NOTICE OF SYNTHETIC COMPLIANCE
footer_title: PLAINTIFF'S NOTICE OF SYNTHETIC COMPLIANCE
jurisdiction: CA
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

# PLAINTIFF'S NOTICE OF SYNTHETIC COMPLIANCE

## 1. Notice

Plaintiff gives notice that every synthetic obligation has been met.

{{person__client}} ({{custom_datetime__verification_date}}).
