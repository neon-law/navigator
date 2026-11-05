---
name: review-pr
description: >-
  Resolve one PR review comment: verify the claim, apply the minimum fix, prove it, reply, and resolve the thread.
---

# `/review-pr`: address a PR comment

[`Address a PR comment`](../../../docs/agent-workflows.md#address-a-pr-comment) is the authoritative procedure. Read it
before acting.

1. Identify the PR and requested comment.
2. Read the PR head, complete thread, cited source, and covering test.
3. Reproduce the claim against the right KIND-backed topology when practical.
4. Adjudicate it as valid, invalid, or valid-but-not-worth-changing, with evidence.
5. Apply only the valid comment's minimum fix and covering test.
6. Run the affected gate and prove the changed behavior.
7. Push, reply with the proof, and resolve only the handled thread.
8. Report unrelated comments, CI failures, and branch state without changing them.

A reply is public, so reference planning by bare issue identifier (`ENG-1234`): never a `linear.app` URL, an issue
title, or a project name. See [Linking a PR to its Linear
issue](../../../docs/agent-workflows.md#linking-a-pr-to-its-linear-issue).

For a dirty tree that needs a new PR, use `create-pr`. For a failed GitHub Action, use [Address a failed GitHub
Action](../../../docs/agent-workflows.md#address-a-failed-github-action).
