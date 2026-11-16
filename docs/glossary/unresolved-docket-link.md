---
title: "Unresolved Docket Link"
description: "A filed document in Docket Filings that no Docket Entry links yet, kept without an invented entry number."
---

An [Unresolved Docket Link](unresolved-docket-link.md) is a [Document](document.md) in [Docket
Filings](docket-filings.md) that no [Docket Entry](docket-entry.md) links. The document keeps its classification, bytes,
visibility, and provenance. The integrity read reports the link as `unresolved`. It does not create an entry or an entry
number.

The Project gate accepts that document under its filed name. Once one numbered entry links it, the [DKT
Number](dkt-number.md) check stays strict: the filename must be that entry's name. A name that disagrees, or more than
one entry linking the same document, still fails the gate.

`navigator project docket renumber` lists the document as unresolved. `--apply` renames nothing and does not rewrite the
bytes.

The state is derived from the entry's document link. It is not a stored flag.

- Read path: [`model::document_pointers::DocketLink`](../../model/src/document_pointers.rs)
