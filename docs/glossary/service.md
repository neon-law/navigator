---
title: "Service"
description: >-
  A Service is one record that one served document reached one recipient by one method on one date, from which a
  certificate of service is composed.
---

One served document × one recipient × one method × one date. A Service is the Project-scoped record that a filed
[Document](document.md) revision was delivered to a party, with the method the rules allow (`personal`, `mail`,
`certified_mail`, `overnight`, `e_service`, or `electronic_by_agreement`), the date, the address or e-service handle
used, the recipient's counsel-of-record role where one exists, and who served it. Where Navigator did the sending, the
record links the evidence that send left: an outgoing [Letter](letter.md) or a sent email.

**A certificate of service is composed, never typed.** A pleading's proof or certificate of service is rendered from
these records the way its caption is rendered from the Project's `matter:` block: the pleading's frontmatter names the
served document as `served_document:` (its slug: the bytes' path below `documents/`), and the renderer reads the
`service:` list on that document's committed [Pointer](pointer.md). With no record, the certificate is omitted from a
preview and a final render refuses, naming the records that are missing. It is never rendered blank, and it never
carries a fill-in line for a date, a method, or a recipient.

**It survives sync.** `navigator project service add|list|show` records and reads a Service through `GET`/`POST
/app/api/projects/{id}/service`, at Lawyer tier and scoped to a participant. `navigator project sync` writes the live
records into the served document's pointer as its `service:` list and keeps a block recorded by hand while the live
Project has none, the same way it carries `signing` and `notarization`. The matter page lists each document's service
beside its revisions, referencing the document by its [Docket Entry](docket-entry.md) where one links it and by its slug
otherwise.

Distinct from a [Service List](service-list.md), which is the set of people a court notice was electronically mailed to
and is conflicts data rather than a record of service by the firm.

- Store: [`store::project_service`](../../store/src/project_service.rs) · Schema:
  [`project_service`](../../store/src/schema/navigator.surql) · Certificate:
  [`word::service`](../../word/src/service.rs)
