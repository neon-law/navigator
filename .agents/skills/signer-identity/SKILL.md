---
name: signer-identity
description: >
  Keep a client's Navigator Person email, the DocuSign signer, and the engagement letter's recipient the same personal
  address. Trigger before sending or registering any DocuSign envelope, when `navigator project envelope register`
  returns 422, when opening a matter, or when a Person still carries a placeholder email. Covers the rule, the pre-send
  check, and the manual parity check until a sync job exists.
---

# One person, one personal email, everywhere

**The client's personal email is the identity.** The Navigator Person, the DocuSign signer, and the Gmail thread must
all use the same address the client gave us. Never leave a placeholder (a plus-address on a Firm domain, `example.com`)
on a Person who will sign.

## At intake

1. Ask the client for the personal email they will sign from. Record it on the Person when the matter opens.
2. A placeholder is a stop sign, not a default. Do not send an envelope while the Person holds one.
3. If no verb can change a Person's email, check `navigator project --help` and `navigator site --help` first, since
   the CLI may have added one. If none exists, record the gap in the matter's handoff note and tell the lawyer.

## Before sending or registering an envelope

1. Read the Person's email on the Project (participants, then the Person row).
2. Read the envelope's signers with `navigator project envelope read <envelope-id-or-link> --host <host>`. A
   `client_not_signer` reason means the Project's client is not among them.
3. They must match exactly, one signer per client plus the signing lawyer. If not, fix the Person first, then send or
   `navigator project envelope register <envelope-id> --project <code> --draft <draft> --host <host>`.
4. A 422 `envelope_adoption` with matching emails is a CLI bug. Report it to Navigator by mechanism, with no names,
   codes, or emails.

## Parity check until the sync job ships

Compare each open Project's client email against the signers of its sent or completed envelopes and list every mismatch.
Report the list to the lawyer. Never change DocuSign contacts or resend an envelope on your own.

## Rules that still apply

* Agents never send email; every message stays an unsent Gmail draft. DocuSign sends are the lawyer's.
* The browser tool may be denied on `docusign.com`, and the lawyer signs in. Never enter a password.
* An issue tracker carries no PII: say "the client's personal email", never the address.
