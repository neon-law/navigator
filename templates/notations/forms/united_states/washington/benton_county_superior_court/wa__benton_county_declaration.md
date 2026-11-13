---
kind: declaration
title: Client Declaration — Benton County Superior Court
respondent_type: person
jurisdiction: WA
origin_url: https://www.courts.wa.gov/forms/?fa=forms.static&staticID=14
confidential: true
# Who signs: the party giving the declaration, in their own name. The
# client signs captive (Navigator serves the embedded ceremony only to
# the notation's bound client), so no `delivery:` key. The identity comes
# from the `person__client` answer — its name and email become the
# `person__client.name`/`.email` context keys the default signer
# bindings read.
signers:
  - role: client
    routing_order: 1
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
    changes_requested: reask__draft
  reask__draft:
    resubmitted: lawyer_review
  generate_pdf__declaration_pdf:
    pdf_persisted: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
    signature_declined: END
  END: {}
---

# DECLARATION OF {{person__client.name}}

I, {{person__client.name}}, declare as follows:

1. I am the client in the matter now pending in the Superior Court of
   Washington for Benton County. I make this declaration from my own knowledge, and the facts stated in it are true.

2. The facts this declaration states are set out in the paragraphs my
   lawyer records below, drawn from what I told them and from the documents in my file.

3. I have read this declaration, and the statements in it are true to my
   own knowledge. I understand that it will be filed with the court and that a party to the case may rely on it.

I declare under penalty of perjury under the laws of the state of Washington (chapter 9A.72 RCW) that the foregoing is
true and correct.

**{{person__client.name}}**

Signature: {{client.signature}}\
Date: {{client.date}}
