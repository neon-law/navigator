---
title: "Reconciliation Signal"
description: "A verified provider event asking Navigator to refresh an already-linked source."
---

A Reconciliation Signal is a verified provider event concerning a source the Firm has already linked. It requests a
fresh read of that source; it is not accounting evidence and cannot authorize a posting or discover another source.
Navigator deduplicates signals in PostgreSQL and gives each a cursor. Reconciliation polling records its cursor in the
audit, can wake early for matching signals, and continues scheduled reads when a webhook is missed.
