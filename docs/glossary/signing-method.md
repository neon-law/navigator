---
title: "Signing Method"
description: "A Signing Method records how an executed Document was signed: wet ink, DocuSign, or Dropbox Sign."
---

A Signing Method records how an executed [Document](document.md) was signed: `wet_ink` for a hand-signed original,
`docusign` for DocuSign, or `dropbox_sign` for Dropbox Sign. It travels with the date it was signed as the optional
`signing` block on the `assets` row and its committed pointer, `signing: { method, signed_on }`. It is recorded at
upload with `navigator site document upload --signing-method <method> --signed-on <YYYY-MM-DD>`, and no gate requires
it.

- Shape: [`store::documents::Signing`](../../store/src/documents.rs)
