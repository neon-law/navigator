# PostgreSQL tenancy contract

The [Firm](glossary/firm.md) owns a [Project](glossary/project.md). A [Brand](glossary/brand.md) is a house identity,
and `firm_brand` records which Brand keys a Firm may use. The Project's selected Brand must be worn by its owning Firm.
A Person can participate in more than one Firm or Project; their system `role` is not a tenant identifier.

| Record class | Scope | Required check |
| --- | --- | --- |
| Firm and membership | Firm | Requested Firm capability |
| Brand and social records | Firm | Brand ownership and the Firm Brand link |
| Matter records | Project | Participation, owning Firm and Brand pair |
| Document and Asset | Project or explicit catalog | Project isolation or explicit catalog provenance |
| Relationship Log and Edge | Project or Firm graph | Scoped traversal; references grant no participation |
| Person and Entity | Identity | Explicit identity links preserve tenant isolation |
| Jurisdiction, Entity Type, Clause | Catalog | Shared lookup; authorized catalog mutation |
| DBOS system records | Infrastructure | DBOS-owned schema and execution access |

The current Surreal schema permits optional `project.firm_id` and optional `brand.firm_id`. The former is a migration
input, not permission for a new unowned Project. Backfill the owning Firm from the persisted Brand assignment, review
any ambiguous assignment, and only then enforce non-null ownership in PostgreSQL. The Brand glossary requires every live
Brand row to have a Firm. An optional SQL field is an incomplete target constraint. Quarantine any unowned Brand during
backfill rather than granting it global visibility.

A Project move must supply the target Firm and Brand together. Reject an unknown Firm, a Brand the target Firm does not
wear, and a cross-Firm update by an actor lacking the target Firm's capability. Never infer a Firm from the actor's only
membership: Owners can administer several Firms, and a Person may belong to more than one. A cross-Brand update within
one Firm still requires the destination Firm Brand link. Preserve Project IDs, document pointers, participation, and
audit provenance during a valid move.

SQL foreign keys prove referenced rows exist. Compound ownership constraints and the `store::firm_capability` check must
also prove they belong together. The application must not rely on row-level security alone: administrative and
background connections can bypass it. See [access model](access-model.md) for route admission, Firm capabilities, and
Project participation.

The complete per-table scope matrix is [ownership matrix](../store/postgres/ownership.json). It names every source
table's ownership path, migration slice and required parity scenario. Fields missing from the source, including Firm
ownership on accounting or graph records, are explicit backfill requirements. The inventory gate rejects both
unclassified source tables and obsolete matrix entries.
