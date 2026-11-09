---
kind: memo
title: Non-Compete Analysis
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  last_day: What is the Person's last day of employment?
  restraint_type: Which restraint does the agreement contain?
  disputed_reason: In the Person's own words, what does the covenant forbid, and what does the Employer say it protects?
choices:
  restraint_type:
    non_compete: Non-compete
    non_solicit: Non-solicitation
    confidentiality: Confidentiality
    none_present: No covenant in the agreement
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: entity__employer
  entity__employer:
    _: custom_datetime__last_day
  custom_datetime__last_day:
    _: custom_single_choice__restraint_type
  custom_single_choice__restraint_type:
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

# Non-compete analysis memorandum

**Client.** {{person__client.name}}

**Employer.** {{entity__employer.name}}

**Last day of employment.** {{custom_datetime__last_day}}

**Restraint.** {{custom_single_choice__restraint_type}}

**The Person's account.** {{custom_text__disputed_reason}}

## Question presented

Whether the restraint the Employer's agreement imposes is enforceable against the Person under New York law, and what
scope the Firm would accept.

## Short answer

Reserved for the reviewing lawyer. The analysis below supplies the basis.

## The governing standard

New York enforces a restrictive covenant only when it is reasonable in time and area, necessary to protect a legitimate
interest, and not a restraint of ordinary competition. _BDO Seidman v. Hirshberg_, 93 N.Y.2d 382 (1999). The Employer
must show the restraint protects trade secrets, confidential customer information, or goodwill the Employer paid for. A
covenant that bars the Person from any role in the field restrains competition; it does not protect a legitimate
interest.

## The factors

1. **Time.** The period the restraint runs. New York accepts a longer term where the Person sold a business than
   where the Person left a job.
2. **Area and scope.** The territory and the forbidden roles. A scope broader than the Person's actual territory or
   duties invites the court to narrow it.
3. **Legitimate interest.** The specific asset the restraint protects. The Firm names it or treats the covenant as
   void.
4. **Hardship.** The restraint must leave the Person a way to earn a living. A covenant that forecloses the Person's
   only trade is an undue hardship.
5. **Partial enforcement.** New York courts may enforce a covenant to the extent it is reasonable and strike the
   rest. _Columbia Ribbon & Carbon Manufacturing Co. v. A-1-A Corporation_, 42 N.Y.2d 496 (1977).

## Recommendation

Reserved for the reviewing lawyer. The deliverable `severance_negotiation_letter__new_york` carries the requested
changes to the Employer.
