---
name: authorization-model
description: >
  Neon Law Navigator's role + participation authorization model, the canonical answer to "who can see what." Every
  person carries exactly one role in `persons.role` (`owner`, `admin`, `lawyer`, `clerk`, or `client`); per-project
  scope lives separately in `person_project_roles.participation`. Owner and Admin bypass project-scoping at route
  admission, not on the matter surface, and only Owner governs Owner identities. Trigger when the user mentions any of
  `role`, `roles`, `owner`, `lawyer`, `client`, `admin`, `participation`, OPA, "who can see", "what does Libra/Nick
  see", or before adding a new authz check anywhere in `web`. Also trigger when reaching for a JSON-array `roles[…]`
  shape: the schema has a single `role` column.
---

# Authorization model: role + participation

> **Role decides the tier; participation decides the scope.**

*What a person is* (system-wide tier) is separate from *what they see* (per-project scope). Both columns live in the
database, not the IdP token. The Rego policy sees only `role`; handlers enforce participation through
`store::access::can_see_project`. [`docs/access-model.md`](../../../docs/access-model.md) is authoritative for the five
tiers and anonymous, the participation vocabulary, the seeded people, and the Rego allow rules.

## Rules

- **One singular `role` column, never a `roles` array.** A person has exactly one `persons` row and one `role`
  (`owner`/`admin`/`lawyer`/`clerk`/`client`). Rewrite any `roles = '["lawyer"]'` or "`session.roles` contains
  `"lawyer"`" prose to `role = 'lawyer'` and `session.role == "lawyer"`.
- **`lawyer` includes attorneys.** Attorney, paralegal, and support personnel are all the `lawyer` tier; there is no
  separate "attorney" role. The difference from `client` shows up in *actions* (edit, sign, file), not visibility.
- **Owner is the highest tier.** The order is `owner > admin > lawyer > clerk > client`. Owner inherits every Admin and
  Lawyer capability. Admin cannot create, edit, or demote Owner identities; only Owner can manage an Owner. Person
  deletion is client-only, so privileged identities are never deleted through that command.
- **Owner and Admin bypass project-scoping at route admission**, except `/app/owner`, which is Owner only. `session.role
  in {"owner", "admin"}` allows every authenticated request with no per-read audit row; the Owner-only carve-out then
  denies Admin on that path. This route-admission bypass does not bypass the matter surface's firm-side participation
  gate. Each tier is its own enum value, not "lawyer + a flag". Admin people and matter directories also filter by
  `person_firm_role`.
- **`participation` is derived from `persons.role`, never entered.** `store::projects::participation_for_role` is the
  only way one is chosen, so the column holds a tier word and nothing else. No write door takes a participation: not the
  lawyer matter-people form, not `POST /app/api/projects/{id}/participants`, not `link_person_project`. Reject a
  proposal to add a matter-side word (`co_counsel`, `translator`): outside counsel is a `lawyer` Person, and an adverse
  party gets no row at all. The Rego policy reads no participation value.
- **Don't conflate `persons.role` with `person_project_roles.participation`.** Same English word, different columns,
  different decisions. `participation` is also not the `disclosures` table (conflicts of interest), which flows the
  other direction.

## Boundaries

- The Rego decision point (embedded policy, `require_policy` middleware): [[opa-policy]] and `docs/rego-policy.md`.
- How the `role` enters the session at login: [[rauthy-oidc]] and `docs/oidc.md`.
