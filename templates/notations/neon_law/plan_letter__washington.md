---
kind: letter
title: Plan Letter
respondent_type: person
jurisdiction: WA
confidential: true
prompts:
  letter_subject: In one clause, what is this letter about?
  response_deadline: By what date should the recipient respond?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: entity__other_party
  entity__other_party:
    _: address__principal_office
  address__principal_office:
    _: custom_text__letter_subject
  custom_text__letter_subject:
    _: custom_datetime__response_deadline
  custom_datetime__response_deadline:
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

{{entity__other_party.name}}\
{{address__principal_office}}

Re: {{person__client.name}} — {{custom_text__letter_subject}}

Dear {{entity__other_party.name}}:

This Firm represents {{person__client.name}}. The Firm has reviewed the documents the Person holds on this subject and
writes at the Person's direction.

The Person asks you to:

1. State in writing the basis for the position your documents do not show.
2. Preserve every record connected to the subject of this letter.
3. Respond in writing by {{custom_datetime__response_deadline}}.

Washington's Consumer Protection Act, RCW 19.86, protects the Person's interests here, and the Firm expects any
resolution to respect it. This letter is not a complete statement of the Person's position and waives no right or claim.

Sincerely,

{{firm.legal_name}}

By: ______________________________  Date: ____________
