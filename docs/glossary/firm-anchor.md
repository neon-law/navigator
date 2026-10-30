---
title: "Firm Anchor"
description: >-
  An internal singleton that identifies the Firm's anchor Entity.
---

Internal plumbing for a [Firm](firm.md)'s [Entity](entity.md). Claiming the singleton prevents concurrent setup from
creating rival firm identities.

The table is `firm_anchor`.

Schema: [`firm_anchor`](../../store/src/schema/navigator.surql#L1158).
