---
title: Billed Call
description: "One completed phone call's billed time: the provider's duration rounded up to six-minute tenths."
---

A **Billed Call** is one completed phone call's time on the record: an idempotent PostgreSQL row keyed on the call's
`CallSid`, written only from Twilio's final `CallStatus=completed` event. The row carries the [Firm](firm.md) the
receiving [Brand Phone Number](brand-phone-number.md) resolves to, the [conversation](communication.md) the call
ingested into, the provider's own duration and start time, and the billed **tenths**: the duration rounds UP to the
six-minute unit, so a one-second call bills one tenth of an hour and a 6:01 call bills two. A zero-duration call — a
voicemail drop, a call that connected for nothing — is recorded with zero tenths, never skipped: the record shows the
client reached out.

Until a lawyer attributes the call's conversation to a Project, the row parks at the Firm level with no `project_id`:
visible, computed, and unbillable. Attribution is the only mutation after the row lands — it sets the Project and
nothing else. Duration, tenths, and the provider's start time never move after the completed event, so the audit trail
always shows the billing as the provider reported it. A redelivered completed event with different call facts conflicts
loudly and leaves the original row intact; a silent overwrite would double-bill a call whose length the provider
disagrees with.

- Schema: `billed_call` in [`store/postgres/migrations/`](../../store/postgres/) ·
  [`store::billed_calls`](../../store/src/billed_calls.rs) · [`portal::twilio_voice`](../../portal/src/twilio_voice.rs)
