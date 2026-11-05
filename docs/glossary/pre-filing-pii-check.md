---
title: "Pre-Filing PII Check"
description: "Refuses a draft carrying a Project's code or participant identifiers before filing outside the store."
---

The pre-filing PII check refuses a draft that carries a Project's code or an attached participant's identifiers before
that draft is filed in an outside tracker. `navigator project feedback-check --project <Project code>` resolves the
matter's code, name, and every participant's name, mailbox, and phone through the site API with the caller's `site
login` session (`--host`, or the Project checkout's host, picks the deployment) and scans the draft against them
alongside any hand-supplied `--term` values. The terms come from `GET /app/api/projects/{id}/private-terms`, which only
a firm-side participant on the Project reads; everyone else gets the same 404 an unknown Project gets. The draft itself
never leaves the machine, and the check exits non-zero naming each identifier to replace. A Project code that resolves
to nothing is an error, never a clean scan — a typo must not read as a clean draft. See
[`cli::projects::feedback_check`](../../cli/src/projects/feedback_check.rs) and
[`cli::feedback_scan::scan`](../../cli/src/feedback_scan.rs).
