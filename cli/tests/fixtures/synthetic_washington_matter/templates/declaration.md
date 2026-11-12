---
kind: declaration
output: pleading
title: Synthetic Declaration of the Respondent (Washington)
caption_title: DECLARATION OF SAM SYNTHETIC TWO
footer_title: DECLARATION OF SAM SYNTHETIC TWO
jurisdiction: WA
respondent_type: person
confidential: false
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: END
  END: {}
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    approved: generate_pdf__declaration_pdf
  generate_pdf__declaration_pdf:
    pdf_persisted: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
    signature_declined: END
  END: {}
---

# DECLARATION OF SAM SYNTHETIC TWO

## 1. Declaration

I, {{person__client}}, declare as follows.

I declare under penalty of perjury under the laws of the State of Washington that the foregoing is true and correct.

{{client.signature}}

{{client.date}}
