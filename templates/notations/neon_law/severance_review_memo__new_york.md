---
kind: memo
title: Severance Review Memo
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  last_day: What is the Person's last day of employment?
  agreement_received: On what date did the Person receive the separation agreement?
  signing_deadline: By what date does the Employer require the signed agreement?
  disputed_reason: In the Person's own words, what does the Employer say happened?
  over_forty: Is the Person forty years of age or older?
  plan_coverage: Does a legal plan cover this Project?
  client_goal: What outcome does the Person want?
choices:
  client_goal:
    negotiate: Negotiate the terms
    accept: Accept the agreement as offered
    contest: Contest the separation
    advise: Advice only, no negotiation
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: entity__employer
  entity__employer:
    _: custom_datetime__last_day
  custom_datetime__last_day:
    _: custom_datetime__agreement_received
  custom_datetime__agreement_received:
    _: custom_datetime__signing_deadline
  custom_datetime__signing_deadline:
    _: custom_text__disputed_reason
  custom_text__disputed_reason:
    _: custom_yes_no__over_forty
  custom_yes_no__over_forty:
    _: custom_yes_no__plan_coverage
  custom_yes_no__plan_coverage:
    _: custom_single_choice__client_goal
  custom_single_choice__client_goal:
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

# Severance review memorandum

**Client.** {{person__client.name}}

**Employer.** {{entity__employer.name}}

**Last day of employment.** {{custom_datetime__last_day}}

**Agreement received.** {{custom_datetime__agreement_received}}

**Signing deadline.** {{custom_datetime__signing_deadline}}

**The Employer's account.** {{custom_text__disputed_reason}}

**Forty or older.** {{custom_yes_no__over_forty}}

**Plan coverage.** {{custom_yes_no__plan_coverage}}

**Objective.** {{custom_single_choice__client_goal}}

## Question presented

Whether the separation agreement the Employer offered is knowing and voluntary, whether its restrictive covenants are
enforceable under New York law, and what the Firm recommends before the signing deadline.

## Short answer

Reserved for the reviewing lawyer. The analysis below supplies the basis.

## Facts

New York law presumes employment at will. The reviewing lawyer records here the events around the separation — the
stated reason, the notice given, the records handed over, and the offer — from the intake answers and the Person's
documents.

## Analysis

### 1. The agreement's release

The Firm checks the release against the claims the Person could actually bring. A claim the release does not cover is
not waived. New York construes a general release by its language, and a release of unknown claims needs plain words. A
release never reaches a claim the law does not allow a person to waive.

### 2. The age-discrimination windows

For a Person forty years of age or older, the Older Workers Benefit Protection Act conditions a release of Age
Discrimination in Employment Act claims on all of the following: a written agreement that refers specifically to those
rights or claims; consideration beyond anything already owed; written advice to consult an attorney; a consideration
period of at least twenty-one days, or at least forty-five days when the separation is part of an employment termination
program offered to a group; and a revocation period of at least seven days after signing. 29 U.S.C. § 626(f)(1)(F)–(G).
A group program also requires the Employer's written disclosures about the class and the ages of those eligible and
selected. The reviewing lawyer confirms which period applies and strikes what does not.

For a Person under forty, those windows do not apply. The review instead tests the release against New York's limits on
discrimination-claim waivers.

### 3. Restrictive covenants

New York enforces a restrictive covenant only when it is reasonable in time and area, necessary to protect a legitimate
interest, and not a restraint of ordinary competition. _BDO Seidman v. Hirshberg_, 93 N.Y.2d 382 (1999). The deliverable
`non_compete_analysis__new_york` carries the full analysis when the agreement contains a covenant.

### 4. Notice and final pay

The Firm checks the notice the Employer owed — ninety days under New York Labor Law § 860-b for a qualifying event,
sixty days under the federal statute for a covered employer — and the final pay statement against New York's
wage-statement rules.

### 5. Unemployment insurance

A layoff is not misconduct. The Employer's stated reason controls the analysis, and the Firm tests it. The hearing
request runs thirty days from the mailing or delivery of the determination, under Labor Law § 620(1)(a). The deliverable
`unemployment_appeal_letter__new_york` answers an adverse determination.

## Recommendation

Reserved for the reviewing lawyer, consistent with the objective recorded above.
