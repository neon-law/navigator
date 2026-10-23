---
title: "Notation Kind"
description: "A Notation Kind is the kind value of a template, which names what it is and picks its default output."
---

The `kind:` frontmatter key names what a Navigator markdown file *is*. It is the only discriminator: the checker reads
it directly and never infers a family from a file's structure or path. `S103` accepts exactly the 16 values below, and
`S104` flags a file that carries notation structure but declares none. Not to be confused with a [Letter](letter.md),
which is one piece of physical mail; `kind: letter` is a template.

Only the twelve **notation** kinds render to a document, and each has a default [Output Format](output-format.md) that
applies when the template declares no `output:`. The rest are content pages and an engineering intake, which are never
rendered through `navigator notation pdf`.

- `letter` — A letter the firm sends on the client's behalf (demand, notice, settlement, closing). Default output:
  `letter`, firm letterhead, even with no `output:`.
- `onboarding` — The engagement that opens a matter and may be its first notation. Default output: `letter`.
- `offboarding` — The firm-signed letter that closes a matter; it may be a pitch's first notation. Default output:
  `letter`.
- `memo` — An analytical work product the firm delivers, such as a review memo or opinion. Default output: `letter`.
- `filing` — A document filed with a government body (formation, annual report, tax return, application). Default
  output: `form`, AcroForm fill.
- `pleading` — Court paper filed with a court (complaint, motion, brief). Default output: `pleading`, court geometry
  from `jurisdiction:`.
- `agreement` — A private agreement between the client and a third party (employment, contractor, operating
  agreement). Default output: `contract`, no letterhead.
- `will` — A last will and testament. Default output: plain, no letterhead.
- `trust` — A trust instrument. Default output: plain.
- `directive` — An advance health-care directive or durable financial power of attorney. Default output: plain.
- `resume` — A person's resume. It needs no `questionnaire:`, `workflow:`, or `jurisdiction:`. Default output: `resume`,
  no letterhead.
- `trial_prep` — An internal trial-preparation Notation whose digest-verified body can supply a lawyer-only `stage_list`
  table. Default output: plain. Its body is work product; the kind catalog does not make that body public.
- `event` — A dated public event page under `server/content/events/`. Default output: not rendered.
- `post` — A blog post file. Posts are database rows now ([Post](post.md)). Default output: not
  rendered.
- `workshop` — A public workshop or teaching page under `web/content/workshops/`. Default output: not rendered.
- `github` — An engineering intake that gathers what a GitHub issue or pull request needs. Default output: not
  rendered.

Further values name a matter page kind (`review_queue_workbench`, `verifier_split_view`, `matter_status_console`,
`docket_deadline_board`, `document_workbench`, `authority_library`, `discovery_cockpit`, `hearing_console`,
`deliverable_package`, `engagement_billing_records`, `estate_plan_console`, `closing_room`). They are declared only on a
`pages:` entry in a Project's `navigator.yaml` (see [the gate](../gate.md#declaring-a-matter-page)); a Markdown file
that declares one fails `S103`.

Seven further values classify a filed [Asset](asset.md) row only (`transcript`, `inbound_contract`,
`certificate_of_naturalization`, `exhibit`, `closed_repository`, `invoice`, `unclassified`); a template that declares
one fails `S103`.

Run `navigator notation kinds` for this table straight from the checker.

- Source: [`rules::kind::Kind`](../../rules/src/kind.rs) · Frame
  derivation: [`Kind::default_output`](../../rules/src/kind.rs)
