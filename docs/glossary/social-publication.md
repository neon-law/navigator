---
title: "Social Publication"
description: "A Social Publication records one Post's delivery state on one social provider."
---

A Social Publication tracks a Post's delivery to one social provider. Its stable identity prevents a repeated job from
creating a second provider post. An asynchronous `provider_task_id` retains a processing receipt separately from its
public post identity; a replay polls that task without sending again. A confirmed result stores the provider's receipt
identifier, and its public link only once the provider's own read endpoint returns one; X uses its documented receipt
URL after readback confirms the Post ID and author. YouTube uses its documented watch URL only after readback verifies
the video ID, selected channel, public visibility, and successful processing. Its periodic reconciler revisits pending
links after the initial checks, including Posts already published on the website. Until verification succeeds, the link
is pending; reading it later never sends the post again. A known failure, including a rate limit or temporary provider
error, may be retried; an ambiguous outcome requires reconciliation before another send. Only an explicit `production`
deployment over real matters sends one: a simulated, development, or misconfigured deployment records the destination as
a known pre-send failure and makes no provider request.

A confirmed publication can then be encoded onto its Post as a link through the Brand's [Social
Handle](social-handle.md).

- Schema and transitions: [`store::social_publications`](../../store/src/social_publications.rs) —
  [`social_publication`](../../store/src/schema/navigator.surql)
