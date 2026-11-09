---
kind: letter
title: Severance Negotiation Letter
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  agreement_received: On what date did the Person receive the separation agreement?
  response_deadline: By what date should the Employer respond?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: entity__employer
  entity__employer:
    _: address__principal_office
  address__principal_office:
    _: custom_datetime__agreement_received
  custom_datetime__agreement_received:
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

{{entity__employer.name}}\
{{address__principal_office}}

Re: Separation of {{person__client.name}} — proposed revisions

Dear {{entity__employer.name}}:

This Firm represents {{person__client.name}} in the separation. The Person received the draft agreement on
{{custom_datetime__agreement_received}}. The Person has not signed it.

The Firm's review is under way. The Firm proposes the following revisions and will negotiate from them:

1. The consideration period and the revocation period comply with the Older Workers Benefit Protection Act, 29
   U.S.C. § 626(f)(1)(F)–(G).
2. The restrictive covenants are limited to what protects a legitimate interest the Employer can name.
3. The release excepts every claim the law does not allow a person to waive.
4. Benefits continuation is stated in the agreement itself, not promised outside it.

The Firm requests the Employer's response by {{custom_datetime__response_deadline}}. This letter waives no right or
claim of the Person and is not a complete statement of the Person's position.

Sincerely,

{{firm.legal_name}}

By: ______________________________  Date: ____________
