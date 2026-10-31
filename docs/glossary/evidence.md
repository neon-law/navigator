---
title: "Evidence"
description: >-
  The provenance a lawyer records for an exhibit: the date it bears, its source, how confidently it was identified, and
  the claim references it supports.
---

The provenance a lawyer records for an [Exhibit](exhibit.md) when filing it: the date the exhibit itself bears (a
letter's own date, a photograph's capture date — never the filing date), where it came from, how confidently the exhibit
was identified, and the claim references in the matter's theory it bears on.

Recorded at upload through the evidence flags, stored as a nested `evidence` block on the [Asset](asset.md) row and on
the committed document pointer, and mirrored to PostgreSQL in the same change. A stored value the running build cannot
read reads back as unrecorded rather than failing the document. Every field is optional: an exhibit with no evidence
block is a gap the [Evidence Index](evidence-index.md) shows, not a broken record.

Schema: [`asset.evidence`](../../store/src/schema/navigator.surql).
