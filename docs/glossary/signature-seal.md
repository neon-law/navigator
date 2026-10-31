---
title: "Signature Seal"
description: >-
  The executed mark a matter page shows on a Notation whose envelope completed, linking its CompletedDocuments.
---

The navigator-signature element a Matter Page renders beside an executed document: proof that the envelope completed,
not that anything else happened. It appears only when the provider's completion webhook has classified an
`envelope-completed` callback — a [Signature](signature.md) row stamped `signed_at` — and its links open the
CompletedDocuments that same webhook archived: the signed PDF and the Certificate of Completion, the ESIGN record.

The seal never reads as executed from storage presence or workflow state alone; an object at a signed-document key with
no completed Signature row is not an execution. See
[`webapp/src/components/signature_seal.rs`](../../webapp/src/components/signature_seal.rs).
