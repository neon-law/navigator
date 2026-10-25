---
title: "DKT Number"
description: "The four-digit, zero-padded court entry number that prefixes a Docket Filings document's name; it equals its Docket Entry's number."
---

The four-digit, zero-padded court entry number that prefixes a [Docket Filings](docket-filings.md) document's name; it
equals its [Docket Entry](docket-entry.md)'s number. An attachment keeps its suffix (`DKT 0029-1`).

A filing is named `DKT NNNN - <title>.<ext>`, flat in `documents/Docket Filings/`, with no per-docket subfolder; its
[Pointer](pointer.md) sits at `blobs/DKT NNNN - <title>.<ext>.yaml`. The title is the entry's text as the docket prints
it, with two changes so the name is a legal macOS file name. Each of `/ \ : * ? " < > |` and every control character
becomes `-`, because `:` and `/` cannot appear in a name and a slash cannot be made legal by quoting; and the title is
cut to 120 characters. The entry itself keeps its title verbatim. Gaps are expected: an entry with no document, such as
an electronically issued summons, has no file, so the numbers on disk skip it.

`navigator project docket ingest` files every document under this name. It still recognises a filing made under the
older `docket/<case>/di-<n>[-<att>]-<slug>.pdf` name, so nothing is filed twice. `navigator project docket renumber`
lists each of those older filings beside the DKT name it would take and changes nothing.

A Project whose manifest lists several `dockets:` shares one flat folder, so two cases with the same entry number
collide on the DKT Number; ingest then treats the later notice as already filed.
