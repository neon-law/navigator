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
