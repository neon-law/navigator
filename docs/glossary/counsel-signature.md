---
title: "Counsel Signature"
description: "A Counsel Signature is the line where counsel e-signs a court filing as /s/ Name."
---

The line in a pleading where [Counsel](counsel.md) e-signs the filing, written `<!-- counsel-signature -->` in its own
paragraph. A draft prints an italic `/s/ DRAFT` on a signature rule drawn as the paragraph's bottom border, with `DRAFT`
highlighted; a final render prints an italic `/s/` and `matter.counsel.name`. It is never routed to an e-signature
provider. Compare [Signature Block](signature-block.md), the placeholder that does become an e-signature field, and
[Signer Box](signer-box.md), a blank completed by hand.

- Parser: [`word::paper::counsel_signature`](../../word/src/paper.rs)
