---
title: "Email Signature"
description: "The configured attorney footer included once in an unsent email draft."
---

An attorney's configured email footer, including the approved confidentiality notice. `navigator mail draft` includes
one copy from `NAVIGATOR_GMAIL_SIGNATURE`, refuses missing configuration and keeps the email unsent for attorney review.
Matching trailing copies are consolidated; other prose, supplied recipients, reply headers and attachment bytes remain
intact.

A matching standalone signature before other text requires review before Gmail access. Clearly quoted history and inline
prose are preserved; an ambiguous body leaves the existing draft untouched.

This is distinct from a document [Signature](signature.md). It creates no signing request and no separate database row.
See [the mail CLI](../mail-cli.md) for configuration and draft-update semantics.
