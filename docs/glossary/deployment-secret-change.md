---
title: Deployment Secret Change
description: "A Deployment Secret Change records an encrypted app-key replacement awaiting a production operator."
---

## Deployment Secret Change

An Owner-authored encrypted replacement for a preset production application key. It is an immutable row in
`deployment_secret_change`, sealed by the deployment's dedicated runtime KMS key. Secret values are never returned to
the browser. Blank inputs preserve deployed values and earlier pending changes.

The Owner downloads encrypted changes from `/app/owner/keys`. A Deployment Operator imports them into production SOPS
through `navigator ops secrets import-social-changes`, reviews the encrypted diff, and applies and restarts the selected
deployment separately. Saving a change does not deploy it, verify provider credentials, or authorize a Brand account.

Firm integration secrets and Brand account OAuth tokens remain separate stores with their existing authorization.

The table is `deployment_secret_change`.

Schema: [`deployment_secret_change`](../../store/src/schema/navigator.surql#L2631).
