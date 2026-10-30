---
title: "Git Access Token"
description: >-
  An internal credential that permits a Person to use Navigator's Git transport.
---

Internal plumbing for a [Person](person.md)'s Git transport access, optionally scoped to a [Project](project.md).
Navigator stores the token's digest, owner, scope, and expiry.

The table is `git_access_token`.

Schema: [`git_access_token`](../../store/src/schema/navigator.surql#L509).
