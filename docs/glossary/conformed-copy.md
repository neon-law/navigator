---
title: "Conformed Copy"
description: >-
  The court-stamped copy of a filed document, which records what was filed and on what date.
---

The court-stamped copy of a filed document, which records what was filed and on what date. It is the court's version,
not the firm's: the submitted revision is what the firm sent, and the conformed copy is what the court accepted.

A conformed copy is filed on its [Project](project.md) as a document of kind `filing` under the slug
`efilings/<transaction id>.pdf`, and its [E-Filing Transaction](e-filing-transaction.md) links it to the submitted
revision. It is never a separate, unlinked upload. A transaction carries one only once its status is `accepted`.
