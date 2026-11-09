---
kind: memo
title: Layoff Next Steps
respondent_type: person
jurisdiction: NY
confidential: true
prompts:
  signing_deadline: By what date does the Employer require the signed agreement?
  client_goal: What outcome do you want?
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
    _: custom_datetime__signing_deadline
  custom_datetime__signing_deadline:
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

# Next steps after a layoff

**Client.** {{person__client.name}}

**Signing deadline.** {{custom_datetime__signing_deadline}}

**Your objective.** {{custom_single_choice__client_goal}}

## Before the deadline

1. Do not sign anything on the day you receive it. The Firm reviews every page first.
2. Send the Firm every document the Employer handed over — the agreement, any release, the WARN notice, the
   benefits notice, and the final pay statement.
3. Tell the Firm when anything changes — a new offer, a new date, a new document.

## The clocks

| What | Window |
| --- | --- |
| Your decision on the agreement | 21 days from the final offer; 45 days under a group program |
| Changing your mind after signing | 7 days after the signature |
| Your unemployment hearing request | 30 days from the mailing or delivery of the determination |

The Firm records these dates on your matter page and reminds you ahead of each.

## Money now

1. File the unemployment claim in your first week without work. Do not wait for the agreement to be signed.
2. Confirm with the Employer where the final check and the benefits notice go.
3. Keep every record the Employer gave you, and everything you send.

## Working for yourself

A person who plans to work for themselves should say so early. The Firm's LLC formation kit handles entity formation,
and the negotiation can ask for a covenant the new plan can live with.
