---
title: "Service List"
description: >-
  A Service List is the set of people a court notice is electronically mailed to, each named with the party they appear
  for.
---

The people a [Notice of Electronic Filing](notice-of-electronic-filing.md) was electronically mailed to. Each entry is a
name, often the party that person appears for (`… on behalf of Creditor …`), and one or more email addresses.

A Service List is conflicts data, not matter participation. `navigator project docket ingest` saves each entry as a
[Person](person.md) keyed by the first email address, and appends one [Relationship Log](relationship-log.md) row per
person per filing, such as `<name> appears for <party> in <case>, per D.I. <n>`. The [Conflict-Check
Graph](conflict-check-graph.md) can later parse those rows into [Relationship Edges](relationship-edge.md).

A Service List never creates a [Person–Project Role](personproject-role.md). Participation follows `person.role`, which
defaults to `client`, so a project role would put opposing counsel on the client side of the matter.
