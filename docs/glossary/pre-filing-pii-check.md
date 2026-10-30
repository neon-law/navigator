---
title: "Pre-Filing PII Check"
description: "Refuses a draft carrying a Project's code or participant identifiers before filing outside the store."
---

The pre-filing PII check refuses a draft that carries a Project's code or an attached participant's identifiers before
that draft is filed in an outside tracker. `navigator project feedback-check --project <Project code>` resolves the
matter's code, name, and every participant's name, mailbox, and phone from the store and scans the draft against them
alongside any hand-supplied `--term` values; it runs locally, never sends the draft anywhere, and exits non-zero naming
each identifier to replace. A Project code that resolves to nothing is an error, never a clean scan — a typo must not
read as a clean draft. See [`cli::pii_audit::private_terms`](../../cli/src/pii_audit.rs) and
[`cli::feedback_scan::scan`](../../cli/src/feedback_scan.rs).
