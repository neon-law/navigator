---
title: "Legal Search"
description: "The private search over public case law and legal-aid research that the Legal Sandbox tools perform for a subscriber."
---

The search the [Legal Sandbox](legal-sandbox.md) performs for a subscriber: public case law through
[CourtListener](https://www.courtlistener.com/) and legal-aid research through Descrybe, dispatched through the
Navigator MCP catalog's read-only tools, `search_case_law` and `search_legal_aid`.

It is **private** in one specific sense, and the `SearchTerms` boundary in `mcp/src/search.rs` is what enforces it: an
outbound provider request carries the search terms and nothing else — never a subscriber identity, a session handle, or
matter facts beyond the words searched. The portal proxies every query server-side; a provider learns what was looked
for, not who was looking.

It is **not privileged**. The sandbox is self-help, and search results are public materials a subscriber could have
found themselves; retrieving them does not create an attorney-client relationship. A search provider's outage degrades
to a "search unavailable" answer — the session continues without it.
