---
title: "Draft"
description: "Draft is the lawyer’s English name for a Template."
---

A **[Template](template.md)** in the lawyer's English. The same authored Markdown file, under the noun said out loud in
the office: reusable source carrying all four parts — **metadata**, **questionnaire**, **workflow**, and **body** — that
a [Notation](../notation.md#notation) is created from. *"Send me the engagement draft"* and *"send me the engagement
Template"* name one file and one row. The full anatomy is taught in [`notation`](../notation.md#template).

A Project repository keeps these files directly under `drafts/`, its repository folder for Drafts. `templates/` is the
alias spelling, accepted with a rename warning; a repository uses one of the two, and both lead to the same Template
rows and versioning rules. A Draft becomes a running Notation, which is a database record and never a file in the
repository.

A Draft is versioned by append rather than by edit, exactly as a Template is: `templates.is_current` marks the live
revision, a change adds a row, and every Notation pins the row it was created from, copying its commit and body digest
so a later edit under `drafts/` cannot change what the Notation says. That is a different mechanism from the asset
lane's [Document Identity](document-identity.md) slug, where a re-upload adds a revision to a living document; authored
source is never re-uploaded over.

Every `/templates` path has a `/drafts` twin. The public gallery paths redirect permanently (308) to the canonical
route, keeping the suffix and query string; the API (`/app/api/drafts/...`, including `POST /app/api/drafts/validate`)
and the admin listing (`/app/admin/drafts`) are served by the canonical handlers under the same authorization. An alias
is not counted as a visit; the canonical request that follows is.

In SQL a read-only `draft` table projects `template` under the Draft name. A projection row has its own id,
`draft:<key>`, and carries `source_template_id`, the canonical `template:<key>` it was computed from; compare that
reference, not the projection's own id, with `notation.template_id`. Every write, version, and Notation pin stays on the
`template` row. In Rust, `store::drafts` is `store::templates` and `Draft` is `Template`, with the canonical ids.

Capitalized Draft is that reusable authored source. Three narrower lowercase uses of "draft" are not this noun and keep
their own meanings:

- The Notation workflow `state` value `draft` — one state in a running Notation's lifetime, beside `lawyer_review` and
  `signed`. A Notation exists through intake and drafting and holds its identity through review and signature; signing
  advances the same Notation rather than creating a second one.
- The [Document Drafts](document-drafts.md) workflow prefix `document_drafts__*`, a system wait state for web-rendered
  review-document rows.
- `review_document.status`, whose default is `draft` — the status of one attorney-reviewed instrument a client reads,
  not of the authored source it was rendered from. Schema: [`review_document` in
  `navigator.surql`](../../store/src/schema/navigator.surql).
