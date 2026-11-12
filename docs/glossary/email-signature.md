---
title: "Email Signature"
description: "The configured attorney footer included once in an unsent email draft."
---

An attorney's configured email footer, including the approved confidentiality notice. `hudson mail draft` includes one
copy in the draft, refuses missing configuration and keeps the email unsent for attorney review. Matching trailing
copies are consolidated; other prose, supplied recipients, reply headers and attachment bytes remain intact.

A matching standalone signature before other text requires review before Gmail access. Clearly quoted history and inline
prose are preserved; an ambiguous body leaves the existing draft untouched.

The table is `mail_signature`. The block is stored as one row per Person — see
[`store::mail_signatures`](../../store/src/mail_signatures.rs) — and the attorney themself is the only writer. The draft
command reads the row when the deployment's stores are reachable and falls back to `NAVIGATOR_GMAIL_SIGNATURE`,
announced, otherwise. The row's `updated_at` names the revision a signed draft's provenance prints.

This is distinct from a document [Signature](signature.md). It creates no signing request. See [the mail
CLI](../mail-cli.md) for configuration and draft-update semantics.
