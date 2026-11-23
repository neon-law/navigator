---
title: "Firm Integration Secret"
description: >-
  An encrypted provider credential held for one Firm integration.
---

Internal plumbing for a [Firm](firm.md) integration. The encrypted value and key version let the integration use its
credential within the Firm's scope.

The table is `firm_integration_secret`.

Twilio auth tokens can have several active versions: a [Brand Phone Number](brand-phone-number.md) pins one specific
secret record. Creating another token leaves that pinned version active. `put_twilio_auth_token` returns the new record
reference, and `revoke_twilio_auth_token_by_ref` revokes one token within its owning Firm. Kind-wide revocation disables
all active Twilio tokens for that Firm. Other secret kinds replace their previous active version when saved.

Schema: [`firm_integration_secret`](../../store/postgres/schema.sql).

The Firm’s Admin DRI can rotate a Stripe credential from a logged-in Navigator CLI. Pipe the value into the command.

```sh
pbpaste | navigator site firm secret set --firm example --provider stripe --account nevada --kind secret --mode sandbox
```

Choose `nevada` or `operating` and `publishable` or `secret`. Staging accepts sandbox keys. Production with real matters
accepts live keys. The invoice issuer reads the selected account’s secret key. Each version records the actor and time.
Use `navigator site firm secret list --firm example` to see key metadata. Use `secret delete` with the same selectors to
revoke the stored credential.
