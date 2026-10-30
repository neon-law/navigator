---
title: "Social Account"
description: "A Social Account connects one Brand to an authorized provider account."
---

A provider account authorized to publish and manage social interactions for one [`Brand`](brand.md). Its OAuth tokens
are encrypted with the owning Firm's runtime KMS key; readers see account identity and granted scopes, never tokens.
Revocation disables Navigator's use of the connection while preserving its audit metadata.

Every provider request re-reads the account, its Brand, and the Brand's Firm, and checks the granted permissions against
the provider's required set. Facebook and Instagram connect through Facebook Login and hold Page tokens issued from a
long-lived user token, which do not expire. A Threads token is refreshed in its last week and a YouTube token from its
refresh token. LinkedIn connects one chosen Company Page after verifying its administrator role and granted scopes; its
expiring authorization requires reconnection. X connects one explicitly chosen username after verifying its provider
identity; its OAuth refresh token rotates inside encrypted storage before publishing. An expired or under-permissioned
connection asks a Brand manager to reconnect it. TikTok Business connects one verified app-specific account identity and
username with publishing and readback permissions; expiring tokens refresh with rotation and the same account identity.

- Schema and credential lifecycle: [`store::social_accounts`](../../store/src/social_accounts.rs) —
  [`store/src/schema/navigator.surql`](../../store/src/schema/navigator.surql)

The table is `social_account`.

Schema: [`social_account`](../../store/src/schema/navigator.surql#L1081).
