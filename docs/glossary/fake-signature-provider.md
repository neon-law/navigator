---
title: "Fake signature provider"
description: >-
  The local signature loop: a FakeSignatureProvider that proves the send → sign → webhook → file path with envelopes on
  disk and no DocuSign connection.
---

The Fake signature provider is the `fake` value of `NAVIGATOR_SIGNATURE_PROVIDER`: a [Signature](signature.md) provider
that keeps every envelope on local disk, serves a local sign page per recipient, delivers completion as a
DocuSign-Connect-shaped HMAC-signed webhook, and files the signed PDF with a certificate of completion. It exists so the
whole signing loop is provable locally and in CI with no DocuSign credentials.

Its envelopes have no legal weight: it refuses to boot in a [production](deployment-environment.md) environment, and
production must select `docusign`. There is no implicit default — an unset value fails boot, so no "send for signature"
button can look real while writing fake envelopes.

See [`SignatureProviderKind`](../../portal/src/signature.rs) for the closed provider vocabulary and
`docs/docusign-esignature.md` for the real provider's credentials.
