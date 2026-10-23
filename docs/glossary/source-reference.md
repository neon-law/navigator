---
title: "Source Reference"
description: "A Source Reference identifies external evidence within its provider and account scope."
---

A Source Reference carries provider, tenant or network scope, account, object type, external identifier, and an optional
sub-object locator. A payment and invoice have different identities. Several references may support one [Journal
Entry](journal-entry.md); a reference cannot authorize changing its quantities. Records live in
`journal_source_reference`.
