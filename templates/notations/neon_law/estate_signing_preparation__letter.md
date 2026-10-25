---
kind: letter
title: Prepare for Your Signing
respondent_type: person
jurisdiction: NV
confidential: true
prompts:
  signing_start: When does the signing ceremony begin?
  signing_end: When is the signing ceremony expected to end?
questionnaire:
  BEGIN:
    _: person__client
  person__client:
    _: project__engagement
  project__engagement:
    _: address__signing_location
  address__signing_location:
    _: custom_datetime__signing_start
  custom_datetime__signing_start:
    _: custom_datetime__signing_end
  custom_datetime__signing_end:
    _: END
  END: {}
workflow:
  BEGIN:
    _: lawyer_review
  lawyer_review:
    _: END
  END: {}
---

{{custom_datetime__signing_start}}

{{person__client.name}}

Re: Preparing for your signing — {{project__engagement.name}}

Dear {{person__client.name}}:

Your signing ceremony is coming up. This note covers what to bring, what to expect, and what to do beforehand so you
arrive ready to execute.

## Bring identification

Bring your passport, or another unexpired government-issued photo ID. The notary and the witnesses must confirm your
identity before anyone signs.

## Date, time, and location

The ceremony begins at {{custom_datetime__signing_start}}.

It is expected to conclude by {{custom_datetime__signing_end}}.

The location is:

{{address__signing_location}}

## Who will be there

Present with you will be your attorney, two witnesses, and a notary. A beneficiary named in your documents should not
attend the ceremony and may not serve as a witness — a beneficiary's presence, or their signature as a witness, can
itself be a ground to challenge the instrument later.

## Review your drafts beforehand

Read your final drafts before the day of the ceremony and send us any questions in advance. The signing itself is not
the place to raise a substantive question for the first time — we want you to arrive ready to sign, not to revise.

Please let us know if anything above conflicts with your availability.

Sincerely,

**Guieb & Shook PC**, trading as **Neon Law**
