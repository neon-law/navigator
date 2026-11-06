---
title: "Pre-Filing PII Check"
description: "Checks local feedback against authorized Project, Person, and Entity identifiers before external filing."
---

The pre-filing PII check refuses feedback containing private identifiers before it is filed in an outside tracker.
`navigator project feedback-check --all-projects` reads the complete Project list visible through the caller's firm-side
participation and resolves each Project by its stable id. It scans the draft title and body locally against Project
codes and names, participating and referenced Persons' display and structured names, mailboxes, phones and contact IDs,
and client and referenced Entities' names, phones, URLs, contact IDs and standardized EINs. Other supplemental metadata
stays out of the response. Generic patterns also refuse contact details, case numbers and long identifiers. Add `--term`
for private names the store does not know. Show the checked text to the lawyer before filing.

The terms come from `GET /app/api/projects/{id}/private-terms` with the caller's `site login` session. Only a firm-side
participant reads that door; others receive the same 404 an unknown Project receives. `--host`, or the checkout's host,
picks the deployment. An incomplete response, failed read, unknown code or ambiguous code refuses the check; it never
counts as clean. The draft never leaves the machine. Findings name categories and replacement placeholders, not the
matched private values.

`--project <Project code>` limits a scan to explicitly selected Projects. Without `--all-projects` or `--project`,
`--term` and generic-pattern scanning work offline. A clean scan is coverage for the identifiers and patterns checked;
it does not authorize quoting matter text or replace lawyer review.

See [`cli::projects::feedback_check`](../../cli/src/projects/feedback_check.rs) and
[`cli::feedback_scan::scan`](../../cli/src/feedback_scan.rs).
