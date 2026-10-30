---
title: "Record citation"
description: "A point in a draft that names where a record says it: a page and line, a paragraph, or an exhibit page."
---

A point in a draft that names where a [record document](authority.md) says what the draft says it says: `Complaint at
4:12–19` (a page and line range), `Roe Declaration paragraphs 6–8` (a paragraph range), `Exhibit A at 3` (a page of an
exhibit), or a shorthand — `Id.`, `Ibid.`, `supra` — that leans on an earlier citation in the same draft. The cite-check
view renders a draft beside the record page it cites and marks each record citation with what its check says, the way
[Authority use](authority-use.md) marks a drafted assertion beside its source.

The parser is `views::record_citation`. It walks a draft once and returns the citations in reading order, each carrying
its character span (so the view can mark the draft text), the typed record it names, the sentence that contains it, and
the document it resolved to. Document names are matched by their drafted shape — capitalized words, optionally joined by
`of` — and resolved through the matter's alias table, so the same draft parses against any [Matter](matter.md)'s
documents without hardcoding spellings.

A shorthand with no earlier citation in the draft is **flagged for review, not rejected**: `Id. at 5` at the top of a
section may legitimately continue a thread the reader reached from elsewhere. The view renders the flag; the lawyer
decides. A drafted name that matches no document in the alias table leaves the citation unresolved — rendered without a
link, never silently guessed at.
