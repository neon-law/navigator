---
title: "Declaration"
description: >-
  A party's own signed statement drafted on a notation template, rendered without firm letterhead or brand marks.
---

A **declaration** is a party's own signed statement — the firm drafts it on a notation template, but the party gives it
in their own name, so the document is the party's, never the firm's. A template declares it with `kind: declaration`,
and its default render profile is `plain` (`Kind::default_output`): no letterhead, contact line, or other brand mark,
and `output: letter` is refused. An affidavit, its sworn twin, follows the same rule. The same rule keeps a contract off
the firm's letterhead (LAW-14).

Distinct from a `kind: pleading` motion or brief, which is court paper rendered through court-calibrated geometry (see
[Pleading paper](../notation-authoring.md#pleading-paper)); a declaration is a party's statement. See [`output:` and the
render profiles](../frontmatter.md#how-the-finished-document-looks-output).
