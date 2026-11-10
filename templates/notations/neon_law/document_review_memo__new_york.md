---
kind: memo
title: Document Review Memo
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  document_kind: What kind of document is it?
  document_received: On what date did the Person receive it?
  document_summary: In the Person's own words, what does the document say or ask for?
  demand: What does the document demand of the Person, and by what date? Write "nothing" if it demands nothing.
  review_goal: What does the Person want from the review?
choices:
  review_goal:
    advise: Advice on the document as written
    negotiate: Changes negotiated before signing
    respond: An answer to a demand
    sign: A decision on whether to sign
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: entity__other_party
  entity__other_party:
    _: custom_datetime__document_received
  custom_datetime__document_received:
    _: custom_text__document_kind
  custom_text__document_kind:
    _: custom_text__document_summary
  custom_text__document_summary:
    _: custom_text__demand
  custom_text__demand:
    _: custom_single_choice__review_goal
  custom_single_choice__review_goal:
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

# Document review memorandum

**Client.** {{person__client.name}}

**Other party.** {{entity__other_party.name}}

**Document.** {{custom_text__document_kind}}

**Received.** {{custom_datetime__document_received}}

**What the document demands.** {{custom_text__demand}}

**The document in the Person's words.** {{custom_text__document_summary}}

**Objective.** {{custom_single_choice__review_goal}}

## Question presented

Whether the document the Person received is consistent with the Person's rights under New York law, and what the Firm
recommends before the Person answers it, signs it, or lets it sit.

## Short answer

Reserved for the reviewing lawyer. The analysis below supplies the basis.

## Facts

The reviewing lawyer reads the document itself and records here what it actually says — the parties, the obligations,
the dates, the exceptions — against the intake answers above. The Person's summary frames the review; the document
controls it.

## Analysis

### 1. What the document asks

The review starts with the demand recorded above. A document that asks for nothing but a signature is reviewed as an
agreement; a document that asks for money, records, or an answer is reviewed as a demand, and the Firm tests each demand
for a source the law or a signed instrument actually supplies.

### 2. The New York rights in play

New York protects consumers through its deceptive-acts statute, General Business Law § 349, and the reviewing lawyer
checks the document's conduct against it. A written contract claim carries a six-year limitations period under CPLR
213(1). The lawyer confirms the period that fits the document before the Person relies on it.

### 3. The deadlines

The demand recorded above sets the pace, and nothing in this memo extends a date. The lawyer confirms every limitation
and response window against primary sources and records what it confirms; the deliverable `plan_letter__new_york`
answers a demand in writing when the Person wants one.

### 4. The plan's frame

The review is plan work at the plan fee. It covers the one document, a written answer, and a letter the Firm drafts at
the Person's direction. Negotiating revisions, contesting a demand in court, and defending a lawsuit are new engagements
the plan does not cover.

## Recommendation

Reserved for the reviewing lawyer, consistent with the objective recorded above.
