---
title: "Closing Trailer"
description: >-
  A line in a pull-request body, such as Closes <ID>, that completes the named issue when the pull request merges.
---

A line in a pull-request body, such as `Closes <ID>`, that completes the named issue when the pull request merges. The
closing words are `close`, `fix`, `resolve`, `complete`, and `implement` with their inflections. `Refs <ID>` links the
issue without completing it. `navigator project gate --ci` refuses a closing trailer on a pull request whose own body
says a rollout, an apply, or a caller pin is still pending. See [Linking a PR to its Linear
issue](../agent-workflows.md#linking-a-pr-to-its-linear-issue) and the [Gate](gate.md).
