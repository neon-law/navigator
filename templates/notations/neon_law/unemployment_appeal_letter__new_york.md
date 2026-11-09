---
kind: letter
title: Unemployment Appeal Letter
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  determination_date: On what date did the Department mail or deliver the determination?
  disputed_reason: In the Person's own words, what reason did the Department give?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: custom_datetime__determination_date
  custom_datetime__determination_date:
    _: custom_text__disputed_reason
  custom_text__disputed_reason:
    _: END
  END: {}
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    approved: END
    changes_requested: reask__client
  reask__client:
    intake_resubmitted: lawyer_review
  END: {}
---

Re: Hearing request — claim of {{person__client.name}}

To the Unemployment Insurance Appeals Center:

This Firm represents {{person__client.name}}. The Department issued a determination against the Person's claim, mailed
or delivered on {{custom_datetime__determination_date}}. The Person requests a hearing on that determination.

The Person's account of the separation follows:

> {{custom_text__disputed_reason}}

The separation was a layoff, not misconduct, and the determination does not state a ground that disqualifies the claim.
The Firm requests a hearing at the earliest available date, by telephone or in person.

This request is timely under Labor Law § 620(1)(a), within thirty days of the mailing or delivery of the determination.

Sincerely,

{{firm.legal_name}}

By: ______________________________  Date: ____________
