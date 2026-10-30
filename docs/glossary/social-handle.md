---
title: "Social Handle"
description: "A Social Handle is the public name a Brand holds on a provider, recorded before a Post links to it."
---

The public name a [`Brand`](brand.md) holds on a social provider: `NeonLawUSA` on X for `neon`. It is not a credential
and not a [`Social Account`](social-account.md): it exists before anyone connects the account, holds no token, and a
Brand has at most one per provider.

A [Post](post.md)'s social link may only name a handle recorded here, so a link can never point at an account the Brand
does not hold. The handle is also what lets the link be completed after the fact: once the provider's post id is known,
the X permalink is `https://x.com/{handle}/status/{id}`.

Neon Law's handles are seed data in [`store/seeds/neon/SocialHandle.yaml`](../../store/seeds/neon/SocialHandle.yaml).
`navigator site blog handle <brand> <provider> <handle>` records another.

- Schema and derivation: [`store::social_handles`](../../store/src/social_handles.rs)

The table is `social_handle`.

Schema: [`social_handle`](../../store/src/schema/navigator.surql#L1069).
