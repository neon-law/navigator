---
title: "LAW-301 research: an ops diagnostic surface for dangling private-term references"
description: "Grounding for an ops diagnostic listing dangling and malformed private-term references per Project."
---

# LAW-301: ops diagnostic for dangling private-term references

Researched against `origin/main` @ `9fe125dc` (worktree branched to
`task/research-law-301-ops-diagnostic-surface-afw5f71b`). Read-only research; the only change on the branch is this
document. The baseline is `docs/research/feedback-check-private-terms-research.md` on branch
`task/research-law-301-feedback-check-500-on-p-k2un6sM2` (commit `7c228d8e`); its §4 candidates B and C are now
partially overtaken by events — PR #568's typed 409/422 landed — and its recommendation ("a new `navigator ops`
diagnostic") is what Nick decided on 2026-11-08, together with two CLI decisions recorded in §4 below. This document
grounds the diagnostic's design in the surfaces that already exist.

## 1. Ops surface map

### 1.1 The CLI `ops` family

`Command::Ops` (`cli/src/main.rs:1030`) dispatches `OpsCmd` (`cli/src/main.rs:3117`): `github`, `forgejo`, `ship`,
`logs`, `surreal-archive`, `postgres`, `deployments`, `secrets`, `email-summary`, `gcp`, `restate`, `doctor`, `dns`,
`host`, `brand-readiness`, `certificates`, `rebrand`, `observability`, `lsp`, `assets`, `cut-release`,
`public-snapshot`, `release-default-tag`, `release`, `notices`, `firms`, `sas`. Three matter for this design:

- **`ops firms doctor`** (`FirmsAction::Doctor`, `cli/src/main.rs:3500-3510`, dispatched at `main.rs:5510`) is the
  closest precedent: a read-only store-integrity report. It connects to "the `SurrealDB` the sourced `.devx/env` names —
  the same store `web` reads" and prints every active Firm whose Admin-DRI standing is missing, multiple, or ineligible
  (`cli/src/firms_doctor.rs:1-17`, `24-44`). The store function is `store::firms::admin_dri_invariant_report`
  (`store/src/firms.rs:1811`). It never repairs; the doc comment names `store::firms::appoint_admin_dri` as the only
  writer (`firms_doctor.rs:8-11`). Authorization is implicit: the operator holds database coordinates. Exit code:
  `FAILURE` when any Firm fails, `SUCCESS` otherwise (`firms_doctor.rs:46-80`).
- **`ops postgres`** (`cli/src/main.rs:3149-3152`): "PostgreSQL mirror parity and reconcile against `SurrealDB`.
  Reports name rows by UUID only" — an integrity diagnostic that deliberately limits what it prints.
- **`ops doctor`** (`cli/src/main.rs` `OpsCmd::Doctor`, ~`main.rs:3271`): read-only `kubectl get` over the cluster —
  a diagnostic named "doctor" that reads live infrastructure, not the store.

### 1.2 Admin-tier API doors

The extractor `AdminSession` (`portal/src/api.rs:1459-1472`) admits a session whose `role.is_admin_tier()` — `Owner |
Admin` (`store/src/persons.rs:171-173`) — and answers `ApiError::Forbidden` to everyone else. Its doc comment explains
why the gate is an extractor: a caller outside the tier is refused before their body is even deserialized
(`api.rs:1450-1457`). Two read-only doors already follow the exact shape a private-terms diagnostic needs:

- **`GET /app/api/project-repositories`** — `reconcile_project_repositories_door` (`portal/src/api.rs:1684-1723`,
  route at `api.rs:394-398`). Its doc comment is the deployment's stated doctrine for deployment-wide reads: "Admin-tier
  only, and the tier is the point rather than a precaution. Every other matter read on this surface is
  participation-scoped … A reconciliation report is that place: the question is about the whole deployment, so it reads
  `store::projects::all` and gates on `is_admin_tier` instead of pretending a per-caller lens is an inventory"
  (`api.rs:1660-1673`). The proportionality test is explicit: "What it discloses is one code and one repository URL per
  matter, and no matter content — which is what makes reading every row proportionate" (`api.rs:1673-1675`).
- **`GET /app/api/project-lifecycle`** — `project_lifecycle_door` (`api.rs:1726-1758`, route at `api.rs:401-405`):
  "This is an oversight read rather than a matter read: it deliberately reads every row and returns only the stable
  code, lifecycle, Firm/brand, and derived `source_state`" (`api.rs:1715-1721`).

Both doors keep their own top-level noun instead of nesting under `/app/api/projects/...` because the projects prefix's
policy rule "admits any authenticated caller up to five segments, so a reconciliation path nested there would be
policy-reachable by a client even though the handler refuses one" (`api.rs:390-393`, repeated at `api.rs:406-411`). A
lawyer-tier caller is refused 403 on both doors by `admin_tier_doors_refuse_a_lawyer_with_403`
(`portal/src/api.rs:13448-13491`).

Two CLI commands already consume admin doors with the caller's `site login` bearer:

- `navigator projects drift` reads `GET /app/api/project-repositories` (`cli/src/projects/drift.rs:474-500`). Its
  comment explains why the admin door and not `GET /app/api/projects`: the participation lens scopes every tier, "Owner
  and Admin included" (`drift.rs:468-473`). On failure it quotes the response body's first line — "this door is
  admin-tier, so the overwhelmingly likely refusal is a caller who is merely lawyer-tier, and a bare status leaves them
  guessing" (`drift.rs:493-499`).
- `navigator project surfaces reconcile` drives `POST /app/api/project-surfaces/{id}`
  (`cli/src/projects/surfaces_reconcile.rs:1-17`): per-Project `ReconcileOutcome` rows, and — directly relevant to C2 —
  "Each Project is reconciled and reported independently: a Firm whose Forgejo is unconfigured is a `skipped` row naming
  the missing coordinate, never a failed run — the sweep continues" (`surfaces_reconcile.rs:15-17`, outcome type at
  `37-53`).

Admin web *pages* are a separate surface (`portal/src/admin.rs:1-5`, `require_auth`-gated router); the doors above are
the API-side model. The `firms` list door also gates on `is_admin_tier` in-handler (`api.rs:5096-5100`).

### 1.3 Integrity-diagnostic precedent, summarized

| Surface | Kind | Auth | Reads | Output |
| --- | --- | --- | --- | --- |
| `ops firms doctor` | store CLI | DB env | `admin_dri_invariant_report` | per-Firm ok/FAIL rows |
| `ops postgres` | store CLI | DB env | mirror parity | "Reports name rows by UUID only" |
| `GET /app/api/project-repositories` | admin door | `is_admin_tier` | all Projects | code + URL per matter |
| `GET /app/api/project-lifecycle` | admin door | `is_admin_tier` | all Projects | code + lifecycle fields |

## 2. Design options for the diagnostic

The reconciliation to reuse is `private_terms_for_project` (`portal/src/api.rs:2083-2177`):
`store::projects::participations_for_project` (`store/src/projects.rs:482-496`),
`store::project_references::for_project` split by `reference.party.kind` (`api.rs:2097-2105`), then
`store::persons::find_by_ids` (`store/src/persons.rs:710-724`) and `store::entities::find_by_ids`
(`store/src/entities.rs:409-421`) — both silently omit missing ids — and the count reconciliation plus metadata/EIN
validation that today refuse the response (`api.rs:2113-2121`, `2144-2164`).

### (a) CLI command + a new admin-gated door

A read-only `GET` door in the `project-repositories` mold — e.g. `GET /app/api/project-private-terms-reconciliation`
(the exact name is user-owned; it must be a top-level noun per the route-placement rule, `api.rs:390-393`) — that runs
the same reconciliation for every row from `store::projects::all` and reports per-Project findings instead of refusing.
A thin CLI command (the `drift.rs` pattern) calls it with the `site login` bearer.

- **Blast radius:** additive and read-only. New route, new `openapi.rs` entry (the `project-repositories` entry,
  `portal/src/openapi.rs:1651-1683`, is the template, including its 401/403/500 rows and the admin-tier authorization
  sentence). No existing handler changes; the only shared-code risk is refactoring `private_terms_for_project` so the
  door and the diagnostic cannot drift — §3.
- **Auth model:** `is_admin_tier`, matching both precedent doors; extend
  `admin_tier_doors_refuse_a_lawyer_with_403` (`api.rs:13448-13491`) with the new route. This is exactly the privilege
  shape Nick described: the CLI may print what it learns, "it's just the diagnostics in the cloud where we want to be
  careful about privilege" — the door gates, the client renders.
- **Test surface:** handler tests beside the private-terms ones (`api.rs:13803-14200`), the 403-lawyer test, the
  CLI's wiremock suite (`cli/src/projects/drift.rs:951-1010` shows the door-failure shapes worth copying), and the
  help/contract surfaces in §5.
- **What a privilege-careful deployment gets:** a zero-credential-footprint diagnostic — the operator authenticates
  as an admin-tier person through the same `site login` session every other command uses; no DB coordinates ever leave
  the deployment; the door's response can be kept to ids, kinds, and field paths (§3).

### (b) Store-level operator script (`ops ... doctor` style)

A `navigator ops private-terms doctor` on the `firms_doctor.rs` pattern: connect with `store::surreal::connect_from_env`
(`firms_doctor.rs:37-42`) and run the reconciliation against the store directly.

- **Blast radius:** smallest — no wire contract, no OpenAPI, no new door.
- **Auth model:** implicit and strongest-privileged: the operator holds raw database credentials. That is the
  mismatch with the decision's posture: the diagnostic would require DB access that the ops doors were introduced to
  avoid needing, and it cannot run from a lawyer's or administrator's workstation — only from a host that holds
  `.devx/env` store coordinates.
- **Test surface:** store-level tests only (`store/src/firms.rs:1989` shows the report-function test pattern); no
  wiremock, no handler tests.
- **Verdict:** viable fallback, strictly worse fit than (a) given Nick's "careful about privilege in the cloud"
  framing — it moves privilege *up* (DB creds) where (a) keeps it *narrow* (admin-tier session).

### (c) Extending an existing admin door

Widen `project-repositories` or `project-lifecycle` to carry private-term findings.

- **Blast radius:** changes two doors' response contracts that shipped with explicit minimality commitments — the
  lifecycle door "returns no matter content" (`api.rs:1715-1721`) and the repositories door discloses only "one code and
  one repository URL per matter" (`api.rs:1673-1675`). Private-term findings carry dangling *person and entity* UUIDs —
  closer to matter content than either door's current payload by its own stated standard.
- **Auth model:** unchanged (both already admin-tier), which is the one advantage.
- **Test surface:** both doors' existing tests and OpenAPI rows would need rework.
- **Verdict:** rejected — it spends accumulated minimality trust to save one route registration.

### Recommendation

**(a)**, on the `project-repositories` precedent: admin-tier extractor gate, top-level noun, per-Project rows, OpenAPI
entry, 403-for-lawyer test. It is the only option where the privilege story matches the decision: the door is the
careful part, the CLI is the printing part.

## 3. Scope of the diagnostic

**Detect all four failure classes.** They are already enumerated by the refusals in `private_terms_for_project`, and the
diagnostic that sees only two of them reintroduces the "repair one, discover the next" loop the diagnostic exists to
end:

1. `private terms: missing Person record` — dangling participation or referenced-person id (`api.rs:2113-2116`);
2. `private terms: missing Entity record` — dangling Project `entity_id` or referenced-entity id
   (`api.rs:2117-2121`);
3. `private terms: invalid Entity metadata` — stored `metadata` is not a JSON object (`api.rs:2144-2148`);
4. `private terms: invalid Entity EIN` — `metadata.ein` present but not `NN-NNNNNNN` (`api.rs:2149-2164`).

For classes 1–2 the finding should name the dangling id and how it is attached (`participation`, `reference:person`,
`reference:entity`, `project.client`) — that tells the operator whether the repair is `remove_participant` (the PR #568
recovery path, `api.rs:14098-14145`) or a reference deletion. For classes 3–4 the finding should name the entity id and
the field path (`metadata`, `metadata.ein`).

**All Projects in one call: yes.** The admin door has no per-matter 404 philosophy to honor — that doctrine
(`api.rs:2067-2075`) exists to hide a matter's existence from non-participants, and the
`project-repositories`/`project-lifecycle` doors already establish that an admin-tier deployment-wide read is
proportionate when the payload is minimal (`api.rs:1660-1675`, `1715-1721`). The trade-off, stated plainly: the response
reveals per-Project integrity state and dangling record UUIDs to any Owner/Admin. UUIDs alone are raw record ids — the
same class of value the private-terms *terms list* treats as private (`api.rs:2122-2134` pushes `person.id`/`entity.id`
into the scan terms) — but they arrive without names, mailboxes, or EINs, which keeps the payload inside the
proportionality line the two precedent doors drew. A healthy Project should appear either as an `ok` row or not at all
(the `project-repositories` door ships both patterns: `project_codes` lists all, findings name only the broken —
`api.rs:1676-1682`); either is defensible, and which one is **user-owned**.

**Suggested response shape** (illustrative, not decided):

```json
{ "project_id": "…", "code": "cruller-estate",
  "findings": [
    { "kind": "missing_person", "id": "…", "attached_as": "participation" },
    { "kind": "invalid_entity_ein", "id": "…", "field": "metadata.ein" } ] }
```

per Project, with an empty `findings` array (or omission) meaning healthy.

**Refactor note:** the four validations should move into a shared store-level function that returns findings rather than
a first-refusal error, with `private_terms_for_project` mapping `!findings.is_empty()` onto the existing
`PrivateTermsConflict`/`PrivateTermsInvalid` errors (`api.rs:2113-2121`, `2144-2164`) so the door's fail-closed behavior
and the diagnostic's report cannot disagree. The baseline doc's warning stands: do not switch the door to skip dangling
references — the `lawyer_dri_people` tolerance precedent (`store/src/projects.rs:502-509`) is exactly the PII-leak risk
the refusals exist to prevent.

## 4. CLI naming and partial coverage (C1 + C2)

**PR #568 already changed part of this.** The typed errors are live: `ApiError::PrivateTermsConflict` renders `409` with
a `private_terms_incomplete` JSON body, and `ApiError::PrivateTermsInvalid` renders `422` with the same body
(`portal/src/api.rs:8309-8310`, `9376-9386`), the OpenAPI contract documents both (`portal/src/openapi.rs:2879-2881`),
and the CLI already distinguishes the classes: `CONFLICT => "a linked Project record is missing"`, `UNPROCESSABLE_ENTITY
=> "stored Entity identifier data is invalid"` (`cli/src/projects/feedback_check.rs:92-97`). The 500 leg of the baseline
doc is therefore settled; what remains is exactly C1 attribution and C2 continuation.

**The current failure path** (`cli/src/projects/feedback_check.rs:72-101`): the per-Project loop over the selected
projects issues `GET /app/api/projects/{id}/private-terms` (`:79-85`), and any non-2xx builds a status-class reason and
returns immediately — `return Err(anyhow!("{reason} ({status}), so privacy coverage is incomplete"))` (`:88-101`). The
message names neither the Project code nor id, and the first failure discards every later Project's terms. `run()`
renders it as `navigator: privacy coverage could not complete: {e}` and exits `2` (`:146-152`).

**C1 — name the failing Project.** In both modes the loop already holds the `VisibleProject`
(`cli/src/projects/surfaces.rs:27-38`) when the read fails; the change is interpolating its `code` (and id for the
ambiguous-code case, where two visible rows can share a code — the disambiguation test at `feedback_check.rs:336-368`
exercises exactly that) into the failure message before returning. Supporting argument already in the codebase: the
visible list comes from the caller's own participation lens — "visible Projects visible through the caller's firm-side
participation" (`docs/glossary/pre-filing-pii-check.md`; lens at `store/src/access.rs` via `api.rs:2067-2070`) — so the
person reading the message already participates in the named matter. Nick's 2026-11-08 decision settles the posture; the
test update is the only subtlety:

- `a_server_failure_reports_incomplete_coverage_without_echoing_private_values`
  (`feedback_check.rs:276-297`) mocks a 500 whose body is `"Synthetic Private Marker"` and asserts the message contains
  the coverage-incomplete phrasing and never the marker. **None of its assertions conflict with naming the Project** —
  the marker is a server body, not a project code — but the test's title states the general no-echo posture, so it
  should gain a positive sibling assertion (the message *does* name `beta`, the failing project's code) and possibly a
  rename, e.g. `…_without_echoing_the_response_body`, so the suite keeps asserting the still-true invariant (server
  bodies never echo) without implying project ids may not appear. `failed_project_ listing_does_not_echo_a_remote_body`
  (`:305-318`) is unaffected: the list read fails before any project is known.
- New tests: single-project 409 names the code; `--all-projects` failure names the first failing project even when
  others succeed.

**C2 — collect, continue, exit 2.** Change `private_terms` to return per-Project outcomes instead of short-circuit
(`Err`): resolved terms accumulate; failures collect as `(code, reason)` rows; the caller decides. The sweep-continues
pattern is already idiomatic here — `surfaces_reconcile.rs:15-17` does precisely this for reconciles. In `run()`
(`feedback_check.rs:137-163`): when failures are non-empty, print first line `coverage incomplete: N matters
unresolved`, list each uncovered matter's code with its reason, still scan against the terms that did resolve (partial
findings beat no findings — the draft is scanned against every healthy matter), and exit `2`. The fail-closed promise is
intact: the check never exits `0` with incomplete coverage (`docs/glossary/pre-filing-pii-check.md`: "An incomplete
response, failed read, unknown code or ambiguous code refuses the check; it never counts as clean"), and the exit-code
contract of `run()` (`:133-136`: 0 clean, 1 findings, 2 unreadable) is unchanged — the banner disambiguates *why* exit 2
happened. Single-project mode has nothing to continue past; C2 is aggregation-only there, exactly as the baseline doc
priced it (§4 C).

One wording caution for the banner: findings printed from a partially-covered scan must not read as a clean scan of the
whole draft; the banner-first ordering (before any `line N: … found` lines) handles that.

## 5. Test surface

- **CLI wiremock suite** (`cli/src/projects/feedback_check.rs:144-412`): the `site()` fixture (`:146-176`) plus
  tests for resolution (`:180`), term-flagging (`:184`), `--all-projects` aggregation (`:223`), the no-echo 500
  (`:276-297`), all-projects refusal (`:299-303`), listing-failure no-echo (`:305-318`), truncated-response refusal
  (`:320-334`), identical-code disambiguation (`:336-368`), unknown/blank code (`:370-380`), and 404 refusal
  (`:393-411`). Extend with: attributed failures (C1, both modes), continue-and-exit-2 with the banner (C2 — the current
  `all_projects_refuses_incomplete_coverage` at `:299-303` asserts `is_err` and will need to assert the new outcome
  shape), and a 409/422-status message test if the reason strings are touched.
- **Handler tests** (`portal/src/api.rs:13803-14200`): firm-side authorization and client 404 (`:13803`), term shape
  (`:13856`), structured identifiers without metadata export (`:13955` — note it already stores a malformed
  `metadata.ein = 123` at `:14027` purely to prove non-export), dangling person/entity refusals (`:14048-14097`), the
  orphaned-lawyer-DRI recovery (`:14098-14145`), and missing attached entity (`:14161-14200`). **Confirmed gap:** the
  two `PrivateTermsInvalid` refusals — invalid metadata (`api.rs:2144-2148`) and invalid EIN (`api.rs:2149-2164`) — are
  asserted nowhere; only the `PrivateTermsConflict` class has direct tests. Whichever PR touches this area should add
  the two 422 tests (the baseline doc's §5 gap, still open at `9fe125dc`).
- **New door tests** (option (a)): route + handler tests beside the above; extend
  `admin_tier_doors_refuse_a_lawyer_with_403` (`api.rs:13448-13491`) with the new path; wiremock door-failure rendering
  tests on the `drift.rs` pattern (`cli/src/projects/drift.rs:951-1010`, including the 403 message that quotes the
  admin-tier refusal).
- **Contract/help surfaces:** `cli/tests/help.rs` asserts the sorted `project` subcommand list (`feedback-check` at
  `cli/tests/help.rs:358`) — a new project subcommand (e.g. `private-terms-check` or `doctor`) must be inserted there;
  `cli/tests/agent_contract.rs:62-71` only requires the string `navigator project feedback-check` in `AGENTS.md` and is
  untouched unless the command is renamed.
- **Docs:** `docs/glossary/pre-filing-pii-check.md` (the continue-and-banner semantics change its "refuses the
  check" paragraph); `portal/src/openapi.rs` needs an entry for any new door (template at `:1651-1683`); and PR # 568's
  precedent says the 409/422 rows are documented — a new door should document its 401/403/500 (and any 4xx) from day
  one.

## 6. Verdict: two PRs

**Split.** The two changes have disjoint blast radius and different review risk:

1. **PR 1 — the diagnostic:** new store-level findings function (§3 refactor), new admin-tier door, `ops`/`project`
   CLI command over it, OpenAPI entry, 403 test, the missing invalid-metadata/invalid-EIN handler tests. Purely
   additive; nothing existing changes behavior; the only refactor risk is keeping `private_terms_for_project`
   fail-closed while its logic is shared.
2. **PR 2 — CLI semantics:** C1 attribution (including the renamed/strengthened no-echo test) and C2
   collect-continue-exit-2 with the banner, plus the glossary edit. This one changes the check's aggregation contract —
   the baseline doc already flagged it as deserving its own review (§4 C) — and it stands alone: a deployment without
   the diagnostic still gets attributable, continuing checks.

Order: PR 1 first, since the production repair path Nick named depends on the diagnostic existing; PR 2 is independent
and could land in either order. This mirrors the baseline recommendation (B + C1 together, C2 separate) updated for the
fact that B (typed 409/422) already shipped in PR #568 — PR 1 *is* the remaining half of "repair the data actionably,"
and PR 2 is C1 + C2 as one reviewable unit.

## 7. Flagged as user-owned

- **The door's route name and whether healthy Projects appear as `ok` rows or are omitted** (§3) — a disclosure
  posture choice on a new admin read.
- **Whether the diagnostic also reports Projects whose `entity_id` or participation ledger is merely *empty* versus
  dangling** — the current reconciliation treats "no participants at all" as valid; a deployment might want that
  flagged. Not priced in this document; the four failure classes are the decided minimum.
- **The new CLI command's name and placement** (`navigator project …` subcommand vs `navigator ops …`) — both
  surfaces exist; `project` puts it beside `feedback-check` in the help list (`cli/tests/help.rs:358`), `ops` puts it
  beside `firms doctor` (`cli/src/main.rs:3500`).
- **PR 2's banner exact wording** — Nick dictated "coverage incomplete: N matters unresolved" on 2026-11-08; the
  placement (first line, before findings) is this document's reading and worth confirming in review.
- **The data repair itself** remains a production-data decision for the deployment operator; the diagnostic only
  names the work.
