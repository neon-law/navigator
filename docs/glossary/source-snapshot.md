---
title: "Source Snapshot"
description: "A Source Snapshot preserves one versioned observation of an explicitly linked source."
---

A Source Snapshot includes its [Source Reference](source-reference.md), retrieval time, provider revision, content
digest, versioned adapter facts, exact asset quantity, status, and private Document pointers. Unknown values remain
unknown. Records live in `journal_source_snapshot`. A missing or pending source cannot prove reconciliation.
