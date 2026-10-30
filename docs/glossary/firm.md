---
title: "Firm"
description: "A Firm is an owning legal practice."
---

An owning practice. The `firm` table is the tenancy boundary for [Projects](project.md) and for the people who work
them: `project.firm_id` points at the practice that owns the matter, and [`Person–Firm Role`](personfirm-role.md)
records who belongs to that practice. A Firm **is** an [Entity](entity.md): `firm.entity_id` is the legal person that
practice is (the seeded practice is `Guieb & Shook PC`). A Firm is not a [Brand](brand.md). Brand is the storefront a
request resolved to; Firm is which practice owns the matter. `firm_brand` records which closed house-brand keys that
practice wears.

`firm.slug` names the Firm's Forgejo organization. Each Project the Firm owns is a repository in it, at
`<firm-slug>/<project-code>` (`store::firms::forgejo::repository_path`): Firm "Example Law PC" owns
`example-law-pc/sample-litigation`. `store::firms::create` derives the slug from `firm.name` once
(`store::firms::slug::derive`): the name is Unicode NFKD-decomposed with combining marks and apostrophes dropped, ASCII
letters are lowercased, and every other run of characters becomes one hyphen. The result has a Project code's shape, at
most 40 characters, and is unique per installation. It must not be a reserved word: Forgejo's own reserved names, such
as `admin`, `api`, and `explore`, and `navigator`. A name that derives an invalid slug, or another Firm's slug, is
refused. Nothing appends a suffix. The slug is immutable: renaming a Firm never moves its organization. SurrealDB holds
the field `READONLY`, and a PostgreSQL trigger refuses an update. Boot backfills a slug for each Firm that predates the
field, and fails on an invalid or colliding one.

The repository `<firm-slug>/library` is the Firm's [Firm Library](firm-library.md): the Drafts and the default retainer
every matter of that Firm starts from.

Distinct from [`firm_anchor`](../../store/src/schema/navigator.surql), which still pins exactly one [Entity](entity.md)
as the Firm-of-record on the [Conflict-Check Graph](conflict-check-graph.md). A deployment may hold several `firm` rows;
it still holds at most one `firm_anchor` claim. `person.role = owner` remains the one system-wide Owner tier; it is not
a value on `person_firm_role`. Owner reaches `/app/owner` to list every practice and its brands. Admin is scoped to the
firms they hold a `person_firm_role` row on: the people directory and the matter directory at `/app/admin` list only
those firms' rows.

Embedded Rego still does not isolate every project or person route by firm. The Owner listing and the two Admin
directories named above do.

`firm.status` is `active`, `suspended`, or `archived`. Owner, or a Firm's own Admin membership, edits a Firm's
name/status/entity, never its slug (`store::firms::update`), changes or removes a person's membership
(`store::firms::update_membership` / `remove_membership`), and detaches a brand key (`store::firms::detach_brand`) —
each gated through the same `FirmCapability::ManageMembership` check `add_membership` already used. None of these write
`is_dri`; only [`appoint_admin_dri`](personfirm-role.md) does, and each membership-removal door refuses a change that
would leave an active Firm without one. Deleting a Firm that still owns Projects is refused. The Firm detail view at
`/app/admin/firms/{id}` (`webapp::firm_show`) is where these are read together: a Firm's own fields, its brands, its
Admin-DRI standing, and every person on it, with an Edit link for whichever caller holds `ManageMembership` on it.

Owner opens a second (or subsequent) Firm at `/app/owner/firms/new`, naming its Entity and its first Admin DRI in one
submission — `store::firms::create`'s own atomic guarantee; the Admin DRI may be an Admin or an Owner. That Firm's Admin
DRI (or Owner) then edits its name, status, and Entity at `/app/admin/firms/{id}/edit`, where Owner alone also transfers
the Admin DRI designation.

The Firm show page also renders a trailing-30-day billing rollup, from `store::xero_invoices::firm_thirty_day_rollup`,
which sums every mirrored Xero invoice issued on one of the Firm's Projects in the last 30 days, in cents, grouped by
brand and by lawyer DRI (a Project with none groups under `"Unassigned"`), and never sums across currencies — a Firm
billing in two currencies gets two independent totals rather than one misleading sum. `webapp::firm_invoice_graphs`
draws the result as two horizontal grouped bar charts (invoiced cents beside paid cents) in inline server-rendered SVG,
following the no-charting-library precedent `webapp::lawyer_dashboard`'s status pie already set. Every label the chart
draws is a brand's or a person's display name (or `"Unassigned"`) — never a Project code, matter name, or Xero invoice
id, which stay out of this surface entirely.

- Schema: [`firm` in `navigator.surql`](../../store/src/schema/navigator.surql) ·
  [`store::firms`](../../store/src/firms.rs)

The table is `firm`.

Schema: [`firm`](../../store/src/schema/navigator.surql#L880).
