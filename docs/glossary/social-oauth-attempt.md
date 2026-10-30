---
title: "Social OAuth Attempt"
description: "A Social OAuth Attempt is a one-use handshake to authorize a Social Account."
---

A short-lived handshake scoped to an actor, Brand, and provider. Navigator stores a hash of state, consumes it once, and
erases the PKCE verifier after authorization. It snapshots the explicitly selected destination. A shared Facebook Login
also snapshots the selected linked Instagram identity; changing a saved selection cannot retarget the handshake. It
contains no Social Account credentials.

- Schema and lifecycle: [`store::social_oauth_attempts`](../../store/src/social_oauth_attempts.rs) —
  [`social_oauth_attempt`](../../store/src/schema/navigator.surql)

The table is `social_oauth_attempt`.

Schema: [`social_oauth_attempt`](../../store/src/schema/navigator.surql#L1107).
