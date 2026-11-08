---
title: "LAW-301 research: feedback-check 500 resolving private terms"
description: "Why `navigator project feedback-check` fails with a server 500 while resolving private terms."
---

# LAW-301: `project feedback-check` 500 resolving private terms

Researched against `origin/main` @ `65a2c900` (branch
`task/research-law-301-feedback-check-500-on-p-k2un6sM2`). Read-only
research; the only change on the branch is this document. PR #568 (`53abc24f`, "distinguish private-term integrity
failures", refs LAW-301) already landed on main; this document maps the remaining failure and what is left to decide.

## 1. What feedback-check does, and what "resolving private terms" means

`navigator project feedback-check` is the Pre-Filing PII Check: it scans a draft (file or stdin) locally for private
identifiers before the draft is filed in an outside tracker
([docs/glossary/pre-filing-pii-check.md](../glossary/pre-filing-pii-check.md)).

Resolution is a two-call exchange, both from the CLI to the deployment's site API with the `site login` bearer
(`cli/src/projects/feedback_check.rs:41-100`):

1. `GET /app/api/projects` via `surfaces::list_visible_projects` (`cli/src/projects/surfaces.rs`), which yields the
   visible Projects' `(id, code)` pairs. Failure here aborts: "authorized Project list unavailable; privacy coverage is
   incomplete" (`feedback_check.rs:46-48`).
2. Per selected Project — every visible Project when `--all-projects` is passed (`codes` empty), otherwise each
   `--project <code>` resolved to its id from the visible list — a `GET /app/api/projects/{id}/private-terms`
   (`feedback_check.rs:72-76`).

The route is registered at `portal/src/api.rs:438-442` and served by `private_terms_door`
(`portal/src/api.rs:2061-2077`):

- Authorization first: `store::access::matter_viewer` must return a firm-side viewer, otherwise a bare 404
  (`api.rs:2066-2070`; the lens itself is `store/src/access.rs:113-149`).
- The Project row is read; a missing one is 404 (`api.rs:2071-2074`).
- `private_terms_for_project` (`api.rs:2079-2160`) then assembles the term set:

  1. `store::projects::participations_for_project` (`store/src/projects.rs:482-496`) — the participation ledger.
  2. `store::project_references::for_project` — adverse/referenced parties, split into person and entity ids by
     `reference.party.kind` (`api.rs:2090-2102`).
  3. `store::persons::find_by_ids` (`store/src/persons.rs:710-724`) and `store::entities::find_by_ids`
     (`store/src/entities.rs:409-422`) — both documented to **silently omit missing ids** from the result.
  4. A count reconciliation: any id with no row refuses the response (`api.rs:2111-2119`).

Every store error on this path maps to `ApiError::Db(String)`, and `ApiError::Db` renders as
`500 {"error":"internal"}` with the detail logged via `tracing::error!` (`portal/src/api.rs:9368-9375`). So the 500 the
CLI sees is either a genuine store fault or one of the four deliberate integrity refusals detailed in §2 — both wear
the same status code.

On the CLI side, a non-2xx response produces "the server could not resolve private terms (500 Internal Server Error), so
privacy coverage is incomplete" and `run()` exits `2` naming "privacy coverage could not complete"
(`feedback_check.rs:85-101`, `feedback_check.rs:147-152`). The check never scans clean on a failed resolution —
that is the designed fail-closed posture. "Cannot complete" is therefore exactly what the code intends; the
defect is that the failure is unattributable (see §3).

## 2. What makes a term private, and why resolution fails

A "private term" is any string the door returns: the Project's code and name, participating Persons' names, mailboxes,
phones and contact ids, referenced Persons' display names, client and referenced Entities' names, phones, URLs, contact
ids and standardized EINs, plus the raw record UUIDs (`api.rs:2086-2160`; glossary,
`docs/glossary/pre-filing-pii-check.md`). Only a firm-side participant may read them — a client-side participant, a
Clerk, and a caller with no participation all receive the same bare 404 an unknown id gets (`api.rs:2056-2060`).

The four refusal points in `private_terms_for_project`, each currently an `ApiError::Db` → 500
(`api.rs:2111-2119`, `api.rs:2138-2157`):

| Refusal | Trigger | Evidence |
| --- | --- | --- |
| `private terms: missing Person record` | Dangling participation or referenced person id | `api.rs:2111` |
| `private terms: missing Entity record` | Project `entity_id` or referenced entity id dangling | `api.rs:2115` |
| `private terms: invalid Entity metadata` | An Entity's stored `metadata` is not a JSON object | `api.rs:2140-2144` |
| `private terms: invalid Entity EIN` | `metadata.ein` is present but not the `NN-NNNNNNN` shape | `api.rs:2145-2157` |

Answering the question as posed: it is a **missing record** (or, for the last two, a **malformed stored value**) —
not a permission rule and not an unhandled exception class. The permission rules work as documented (404s, exercised by
`private_terms_answer_a_firm_side_participant_and_refuse_the_client`, `portal/src/api.rs:13786`).

PR #568's own repro test proves the dominant case end to end:
`private_terms_recover_after_an_orphaned_lawyer_dri_is_removed` (`portal/src/api.rs:14087-14145`) adds a participant,
deletes the `person` row with a direct `DELETE $person` (bypassing the command boundary), observes the 500, then
`remove_participant` and observes 200. That matches the commit message: "The underlying record integrity failure still
needs diagnosis and repair by the deployment operator."

How the dangling row gets there: the in-band path cannot produce it. `delete_person` only deletes `Role::Client` records
and refuses while any participation exists (`store/src/people_commands.rs:511-537`). So the orphan comes from an
out-of-band write — direct database maintenance, an admin tool, or a code path that deletes a person without cleaning
the ledger. This is a **data problem in specific projects**, amplified by a **server design choice** (integrity
refusals ride the 500 class), not a CLI misuse: the CLI sends the right params and fails closed.

## 3. Server bug, CLI misuse, or data problem?

It is a data problem in the deployment, surfaced through two deliberate but compounding design choices:

1. **Server:** the integrity refusals are correct to refuse (the handler's comment is explicit — "A dangling
   attachment refuses the response instead of claiming a complete check", `api.rs:2079-2082`) but they are typed as
   `ApiError::Db`, the generic internal-fault class (`api.rs:9368-9375`). The client-visible body is the
   unattributable `{"error":"internal"}`; the reason lives only in server logs. The OpenAPI contract documents only
   200/401/404 for this door (`portal/src/openapi.rs:2856-2887`) — the 500 path is undocumented.
2. **CLI:** the error is also unattributable on its side. In `--all-projects` mode the per-project loop aborts on the
   first failure and the message names neither the Project id nor the code (`feedback_check.rs:72-101`). A firm with
   one broken matter out of hundreds cannot tell which one to repair without reading server logs or bisecting with
   `--project` calls.

Both choices are defensible individually; together they turn one dangling row into an unactionable "the check cannot
complete."

## 4. Fix candidates and blast radius

### A. Repair the data (deployment operator) — required regardless

Query the participation ledger and project references for ids with no row, then repair: `remove_participant` for the
orphaned lawyer DRI (PR #568's test shows recovery, `portal/src/api.rs:14130-14141`) or restore the record. No code
change, no API change. The server log line "private terms: missing Person record" (`api.rs:9368-9375`) already carries
the static reason per failing request. **Gap:** nothing today lists *which* Project or *which* id is dangling from
outside the server logs — the operator must correlate log timestamps with requests (user-owned decision, §6).

### B. Server: type the integrity refusals as a client-visible error

Give the four refusals their own `ApiError` variant (the codebase precedent is `ForgejoUnavailable(String)` — a
non-bare internal fault reported per Project, `api.rs:8436-8440`) rendering, say,
`409 {"error":"private_terms_incomplete"}`, and document it in `openapi.rs`. Blast radius: the door's wire contract
gains a documented non-2xx; the only callers are
the CLI and the five handler tests. The CLI then maps it to "the server has an incomplete identifier set for this
matter; operator repair required" instead of "the server could not resolve." Trade-off: an attacker-visible distinction
that the records behind a matter are inconsistent — a small disclosure, and the reason this probably wants a bare
project-id-free body (the 404 philosophy at `api.rs:2056-2060` exists for exactly this caution). The refusal still fails
the check closed; nothing scans clean on it.

### C. CLI: attribute the failure and (optionally) continue per-project

Two separable sub-changes, both in `cli/src/projects/feedback_check.rs`:

- **C1 (small, safe):** name the Project in the per-project failure message before `return Err`
  (`feedback_check.rs:78-100`).
  Judgment call, flagged: Project codes are themselves private terms (§2), so the message would echo a private value to
  the operator running the command — the same person the door is disclosing terms to anyway. PR #568's test
  (`a_server_failure_reports_incomplete_coverage_without_echoing_private_values`,
  `feedback_check.rs:277-297`) asserts the message never echoes the server body, not the project id; adding the id
  touches that privacy posture and should be a deliberate review decision.
- **C2 (semantic change):** in `--all-projects` mode, collect per-project failures, scan against the terms that did
  resolve, and exit `2` listing the uncovered matters. The check still never passes incomplete (the glossary's promise,
  `docs/glossary/pre-filing-pii-check.md`), but it is no longer all-or-nothing: one broken matter no longer hides
  findings in every other matter's draft. Trade-off: partial coverage is a footgun if a caller treats a long findings
  list as "the check ran"; mitigated by keeping exit `2` and a first-line "coverage incomplete: N matters unresolved"
  banner. For single-project mode there is nothing to continue past — C2 is aggregation-only.

### Recommendation

A first (it is the actual repair and it is user-owned: production data). Then B + C1 as the minimal code fix — they
turn the next occurrence into an attributed, actionable failure without changing when the check refuses. C2 is worth
taking for `--all-projects` ergonomics but changes the aggregation contract and deserves its own review. I would not
skip dangling references server-side (the `lawyer_dri_people` precedent tolerates missing persons,
`store/src/projects.rs:497-509`): a silently skipped private name is a PII leak the current design correctly refuses
to risk.

## 5. Test surface

- **CLI unit tests** (`cli/src/projects/feedback_check.rs:134-408`, wiremock): resolution shape, `--all-projects`
  aggregation, identical-code disambiguation, and PR #568's 500-no-echo test (`a_server_failure_reports_incomplete_
  coverage_without_echoing_private_values`, `:277`). Extend for: attributed project id in the failure (C1), and
  continue-past-failure aggregation with an exit-2 banner (C2).
- **Server handler tests** (`portal/src/api.rs:13786-14145`): firm-side authorization and 404 refusal, term-shape and
  metadata-export bounds, dangling person/entity refusals, and PR #568's orphaned-DRI recovery test. Extend for: the new
  error variant's status and body if B lands, and the invalid-metadata / invalid-EIN refusals (currently asserted
  nowhere — only the missing-record refusals have direct tests).
- **Contract/help surfaces:** `cli/tests/help.rs:358` (command listing — untouched by any candidate) and
  `cli/tests/agent_contract.rs:67` (the contract requires the check be run on feedback drafts). `docs/glossary/
  pre-filing-pii-check.md` and `portal/src/openapi.rs:2856` both need edits if B lands.

## 6. Flagged as user-owned

- **The data repair itself** (candidate A): production data in a deployment I cannot see from here. The operator query
  and repair path (direct DB or a new `navigator ops` diagnostic) is a decision for Nick.
- **C1's privacy posture:** whether the CLI failure message may name the failing Project (its id, or its code — the
  code is a private term itself).
- **C2's aggregation contract:** whether partial-coverage scanning is acceptable UX for `--all-projects`.
- **B's disclosure trade-off:** whether a distinct client-visible error class for "integrity incomplete" reveals too
  much to a non-firm-side caller (the door already 404s non-firm-side callers before reaching it, so the exposure is
  limited to firm-side participants — but that is a lawyer's call).
