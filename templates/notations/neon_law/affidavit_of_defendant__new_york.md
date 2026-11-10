---
kind: filing
title: Affidavit of Defendant
respondent_type: person
jurisdiction: NY
confidential: true
signers:
  - spouse
prompts:
  index_number: What index number has the clerk assigned?
  served_on: On what date was the defendant served?
  county_of_residence: In which county will the action be filed?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: person__spouse
  person__spouse:
    _: custom_datetime__served_on
  custom_datetime__served_on:
    _: custom_text__county_of_residence
  custom_text__county_of_residence:
    _: custom_text__index_number
  custom_text__index_number:
    _: END
  END: {}
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    approved: generate_pdf__affidavit
    changes_requested: reask__client
  reask__client:
    intake_resubmitted: lawyer_review
  generate_pdf__affidavit:
    pdf_persisted: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
    signature_declined: END
  END: {}
---

# Supreme Court of the State of New York

**County of {{custom_text__county_of_residence}}**

## AFFIDAVIT OF DEFENDANT

{{person__client.name}}, Plaintiff,

against {{person__spouse.name}}, Defendant.

**Index No.** {{custom_text__index_number}}

{{person__spouse.name}}, being duly sworn, deposes and states:

1. I am the Defendant in this action for divorce.
2. I was served with the summons and complaint, or with the summons with notice, on {{custom_datetime__served_on}}.
3. I have read the summons, and the complaint if one was served. I understand what the action asks: that the marriage
   between the parties be dissolved.
4. I have read the plaintiff's sworn statement that the relationship between the parties has broken down irretrievably
   for a period of six months or more. I do not dispute that statement.
5. I do not intend to appear in this action or to contest it. I consent to the entry of judgment dissolving the
   marriage on the papers, and to the notice of entry by mail to my address.
6. I understand that this affidavit is signed under penalty of perjury and that a court may act on it.

Dated: {{spouse.date}}

Signature of Defendant: {{spouse.signature}}

Sworn before: ____________________
