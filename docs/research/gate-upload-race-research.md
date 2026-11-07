# LAW-284 research: the concurrent-PR slug race in the gate's document/seed paths

Read-only research against `origin/main` at `3c89321` (#525). No code changed.
Every claim points at a file and line inspected this session unless marked
otherwise. Prior art: `docs/research/gate-documents-resilience-research.md`
(LAW-285, branch `task/research-law-285-project-gate-yml-docume-seOXFW7c`,
commit `ff6c892`), read in full this session; its "LAW-284 context" section
named this race as out of scope.

## Scope correction to the issue title

The title — "an upload from a concurrent PR fails cd on whichever PR merges
second" — implies the gate uploads documents. It does not, and never has:

- The `documents` job runs `navigator project gate --check --ci`, and `--ci`
  "writes nothing" (`.github/workflows/project-gate.yml:118-131`;
  `cli/src/projects/document_check.rs:151-157`).
- The `verify` job's gate "writes nothing" under `--ci` too
  (`.github/workflows/project-gate.yml:122-131`).
- The `seeds` job is the only live write in the gate family, and it uploads
  seed records, not documents (`.github/workflows/project-gate.yml:157-179`).
- Portal publication is retired; `project-publish.yml` is a deliberate no-op
  kept so existing `cd.yml` callers keep passing
  (`.github/workflows/project-publish.yml:8-11, 33-46`).

So the race is: **a document upload from any concurrent writer** — a lawyer
running `navigator project sync`/`documents upload` from the winning PR's
checkout (sync "refiles a reconciled document" during project sync), a manual
upload, or a seed reconciliation on a main push — **mutates the live chain,
and the losing PR's read-only `documents` check then disagrees with the live
deployment.** What "cd" shows is the failure of the required `ci` job via a
pointer-drift verdict, not an upload error. Both halves (the write tie-break
and the read verdict) are in scope below.

In this codebase "cd" means the caller Project's continuous-deployment
workflow (`cd.yml`, which calls `project-gate.yml` then the no-op
`project-publish.yml`); the workflow's own comments use "fail cd" that way
(`.github/workflows/project-gate.yml:190-193, 214-216`).

## 1. The exact failure path

### The write side: how two concurrent uploads of one slug collide

The upload door (`POST /app/api/projects/{id}/documents`,
`portal/src/api.rs:5354`) validates, reads the live chain with
`store::assets::revisions`, then calls `store::assets::file_revision`
(`store/src/assets.rs:2260`). The write path is read-modify-write with no
transaction and no lock:

1. `file_revision_inner` reads the current head via `current()`
   (`store/src/assets.rs:2318-2321`), which is
   `ORDER BY id DESC LIMIT 1` over the slug chain
   (`store/src/assets.rs:1088-1110`) — ids are UUIDv7
   (`store/src/documents.rs:1703`: `asset_id.unwrap_or_else(Uuid::now_v7)`),
   so **insertion order is the chain tie-break**.
2. Against the head it enforces: Rule 2, kind immutable across the chain;
   Rule 3, identical bytes change nothing — `Filed::Unchanged`
   (`store/src/assets.rs:2334-2349`).
3. It then inserts a fresh row (`ingest_bytes_as_inner`,
   `store/src/documents.rs:1575-1654`: storage `put` **before** the row
   insert — "the ordering is what carries that, not a transaction") and
   demotes every other revision to `internal`
   (`demote_superseded`, `store/src/assets.rs:2384-2424`).

The collision points, from the module's own admission:

- "**The key-value layer is optimistic, so a write can lose a race**" and
  "**a multi-statement query is not one transaction**"
  (`store/src/assets.rs:23-31`). SurrealDB "has no advisory lock"
  (`store/src/entities.rs:19`).
- `(project_id, slug)` is **deliberately non-unique** on the asset lane —
  `slug: Option<String>` doc, "`Some` makes this row one revision of a living
  document" (`store/src/assets.rs:88-97`). The `UNIQUE (project_id, slug)`
  in `store/postgres/schema.sql:1161` belongs to the `matter_page` mirror,
  not `asset`. **Nothing in the active engine serializes two appends to one
  chain.**

Consequences of two concurrent uploads of the same slug:

- **Different bytes** (the PRs conflict on content): both pass the head read,
  both insert. Two rows enter the chain; UUIDv7 insertion order elects the
  operative revision. The upload whose bytes lost the tie-break silently
  becomes a superseded, demoted-to-internal revision — no error, anywhere.
- **Identical bytes** (e.g. a retried or re-run upload): Rule 3 dedupes only
  against the head **read before either insert lands**; both uploads see the
  old head and both insert, so the chain gains two rows with the same
  `sha256_hex`. Cross-run, same-bytes idempotency is broken; the batch
  uploader's digest guard is per-process only (`completed`/`uncertain` maps,
  `cli/src/remote.rs:2635-2652`), and its slug guard refuses only same-run
  duplicates — "unfiled: slug collision"
  (`cli/src/remote.rs:2614, 2666-2672`).
- The manual buffered upload is one POST with **no idempotency key**; its own
  error context says "upload outcome unknown; reconcile with
  `navigator project gate --check` before retrying"
  (`cli/src/remote.rs:1796-1808`). A retried upload after an ambiguous
  timeout is exactly the concurrent writer that produces the duplicate above.

The one lane that already solves this — mail-source filing — is the
counterexample proving the fix pattern exists: `ingest_bytes_with_mail_
provenance_once` elects one writer with a **deterministic asset id**
(`stable_asset_id(project_id, slug, "immutable-mail-source/v1")`) and, when
the insert loses, re-reads and validates instead of failing
(`store/src/documents.rs:1494-1521`: "A deterministic asset id elects one
database writer across overlapping requests"). Document revisions have no
such election — their ids are random v7 (`store/src/documents.rs:1703`).

### The read side: the concrete error the losing PR's cd shows

The losing PR's `documents` job compares committed `blobs/<slug>.yaml`
pointers against the live chain (`reconcile_slug`,
`cli/src/projects/document_check.rs:634-723`). It computes the desired
pointer from the live chain's operative revision (`desired_pointer`,
`document_check.rs:716-760`) and compares it to the committed pointer. When
the winning writer's upload extended the chain:

- The committed pointer names the older revision, so
  `pointer.as_ref() == &desired` fails and the check records the fix
  `REWRITE = "rewrite the drifted pointer"` (`document_check.rs:29`).
- Under `--ci` (write=false), a fix **fails the job**: `Outcome::rejects`
  returns true when `ci && !fixes.is_empty()` (`document_check.rs:87-90`),
  and `append_document_check` renders each fix as an error
  "would **rewrite the drifted pointer**; run `navigator project gate
  --check` and commit the result" (`cli/src/main.rs:8149-8196`).
- `run_document_check_gate` prints the recap and returns exit code 1
  (`cli/src/main.rs:8037-8064`); the required `ci` job needs `documents`
  success (`.github/workflows/project-gate.yml:242-270`), so the merge is
  blocked — that is the failing cd on the PR that merged second.

The same run can also show the sibling verdicts of this failure family:
"no live document for this pointer; run `navigator project sync` to upload
it or remove the pointer" (`document_check.rs:385`), "the live chain has no
operative revision" (`document_check.rs:729`), "two committed pointers name
`{slug}`" (`document_check.rs:523`), or — for matter-record collisions rather
than documents — the live-coordinates disagreement seen in
`neon-law/sample-litigation` run `40941026632` (cited in the LAW-285 research,
§"Failure signatures"). A pointer removed in the losing PR adds "its pointer
was removed in {commit}" (`document_check.rs:645-656`).

Note the check is not point-in-time consistent even without retries: pages of
the integrity report and per-slug revision reads at concurrency 8
(`REVISION_CONCURRENCY`, `document_check.rs:129`) observe the deployment at
different instants, so a check straddling a concurrent upload can pass on
stale state (the upload lands after the read) or fail on fresh state. The
race is a TOCTOU window, not a deterministic verdict.

## 2. Does LAW-285's retry work change or worsen the race?

PR #524 (`8cb612e`) added `retry_gate_call`, `AttemptOutcome`, and
`transient_status` (500 and 502-504; every 4xx terminal) with per-request
timeouts (`cli/src/remote.rs:637-712`), applied to the OIDC fetch, both
token mints, the integrity pages, and — materially here — the per-slug
revision reads (`list_revisions`, `cli/src/remote.rs:1318-1369`: 3 attempts,
250 ms × attempt, 401 terminal).

- **It does not worsen the write race.** No upload path gained a retry: the
  buffered upload is still single-attempt (`remote.rs:1741-1808`), and the
  LAW-285 research explicitly conditioned any upload retry on an idempotency
  story that still does not exist. Retried calls are reads and mints only —
  both idempotent.
- **It makes the drift verdict more deterministic, not less.** A revision
  read that used to die on one transient 503 now retries and re-queries the
  live store; the retry observes the state as of the retry instant. So a
  check that previously red-failed on transport now proceeds far enough to
  produce the "would rewrite the drifted pointer" verdict against the
  winner's upload. Net effect on cd: roughly the same red rate for true
  collisions, fewer reds for pure transport noise.
- **It marginally widens the read interleaving window.** Up to two extra
  250-500 ms waits per failing call, across concurrent slug reads, spreads
  the instants at which the check samples the chain further apart. That
  degrades point-in-time consistency slightly — more chances to straddle a
  concurrent append — but the window is milliseconds against a
  human-paced merge cadence, and the direction of the error (pass on stale,
  fail on fresh) is unchanged.
- **A retried read cannot mask the collision.** Retries only re-read; they
  never suppress the drift comparison, and a passed check that the winner's
  upload lands just after is the pre-existing TOCTOU, not a retry artifact.

Verdict: LAW-285 neither fixes nor materially worsens the race. It converts
some transient-red documents jobs into drift-verdict jobs, which is progress
toward honest signals, and leaves the write side exactly as it was.

## 3. Serialization primitives already in the codebase

- **Postgres advisory locks** (all PG-side, keyed per resource):
  `pg_try_advisory_xact_lock(hashtextextended($1, 168))` to serialize billing
  retries and conflicting Project choices (`store/src/billed_calls.rs:223,
  250, 307`); a per-notation advisory lane with commit-scoped release
  (`store/src/notation_machine_state.rs:9-14, 108, 261`); startup-migration
  lock (`store/src/journal_startup_migration.rs:155`); FI import
  (`store/src/fi_import.rs:676`).
- **Deterministic-id writer election** on SurrealDB: `stable_asset_id` +
  insert, and on loss re-read-and-validate
  (`store/src/documents.rs:1494-1553`). This is the only primitive that works
  on the asset lane's engine today.
- **Conditional-write precedent** in the upload door itself: a transcript's
  `derived_from` is validated against the live chain — head id, version
  count, and digest must match, else "The source revision is no longer
  operative on this matter" (`portal/src/api.rs:5420-5447`). The
  compare-and-append shape is already expressed; it is just applied to
  transcript derivation, not to ordinary revision appends.
- **Optimistic retry wrapper**: `writing()` + `crate::surreal::retry`, the
  crate's one retry policy for lost writes (`store/src/assets.rs:26-29`) —
  retries a statement, cannot make a read-modify-write sequence atomic.
- **Natural-key idempotent upsert**: seeds "re-run is a no-op on the natural
  keys of each table" (`store/src/seed.rs:3-8`) — the serialization pattern
  the `seeds` job relies on, absent for document chains.
- **Pointer versioning material**: the committed pointer already carries
  `PointerVersion { version, asset_id, created_at, sha256, size_bytes }`
  (`store/src/document_pointers.rs:343-360`), and revision summaries carry an
  `operative` flag and one-based `version` (`portal/src/api.rs:6793-6795`).
  The client has everything needed to distinguish "names an operative
  revision that is no longer head" from "names bytes the chain no longer
  holds".
- What does **not** exist: any unique constraint or advisory lock on the
  SurrealDB asset lane, any transaction spanning the head read and the row
  insert, and any CAS field on the upload doors.

## 4. What LAW-146/LAW-157 history says about this failure family

Both issues hardened the **read** side of the same surface and were landed as
CLI-only changes:

- **LAW-146** — the ~380-pointer matter: integrity reads became bounded
  pages (50 rows, deep 10) merged client-side, so no single request carries
  the whole matter (`cli/src/projects/document_check.rs:2035-2077`; test
  `a_large_matter_is_fetched_in_bounded_pages_and_fully_checked`; server side
  `portal/src/api.rs:9747`). Lesson: the family's first fix was sizing, not
  locking.
- **LAW-157** — a gateway that gives up on a page is reported as a timeout
  naming the page, status, attempts, and elapsed time, never the gateway's
  HTML (`cli/src/projects/document_check.rs:2135-2160`;
  `cli/src/remote.rs:8787`; `portal/src/api.rs:9747`; test
  `a_gateway_timeout_is_reported_as_a_timeout`). Lesson: the second fix was
  diagnostic honesty for read failures.
- **LAW-285** (PRs #522, #524) — retries and timeouts for the gate's remote
  calls. Lesson: the third fix was transport resilience, again read-only.

The pattern across all three: this failure family has been treated three
times as a *client read-resilience* problem, and every fix landed in the CLI.
The write side — chain appends, their tie-break, and upload idempotency — has
never been hardened, and the CLI's only cross-writer defense is a per-process
slug set ("unfiled: slug collision", `remote.rs:2666-2672`) that cannot see
another process. LAW-284 is the first issue in the family aimed at the
server-side write.

## 5. Candidate fixes, with tradeoffs and surfaces

**(a) Server-side serialization of chain appends.** Make the upload door
serialize per `(project_id, slug)` before the head read: either a SurrealDB
transaction spanning read-head + insert, or (more in keeping with the PG-side
precedents) an advisory-lane lock if the write ever moves to Postgres.
- Touches: portal upload doors + store (`store/src/assets.rs`,
  `store/src/documents.rs`, `portal/src/api.rs`). Workflow YAML untouched.
- Wins: closes the different-bytes tie-break ambiguity and the same-bytes
  duplicate at the root; every caller benefits, CLI or not.
- Costs: SurrealDB "has no advisory lock" (`store/src/entities.rs:19`) and
  multi-statement queries are not transactions today, so this either depends
  on engine transaction support the codebase does not yet use on this lane,
  or becomes a distributed-lock design. Highest engineering risk of the
  three.

**(b) Conditional append (compare-and-swap) with client restart.** The
upload request carries the expected head (`asset_id`, `version`, `sha256` —
the exact triple `derived_from` validation already checks); the server
re-validates inside the insert; on mismatch it returns a 409 naming the
current head, and the CLI restarts: re-read, Rule 3 dedupe re-runs (same
bytes become `Filed::Unchanged`), re-append.
- Touches: portal upload doors (`upload_document_door`, streaming complete),
  store (`file_revision` signature), CLI (`upload_bytes_buffered/streaming`,
  `file_upload_target`). Workflow YAML untouched.
- Wins: generalizes the mail lane's election and the `derived_from` check
  into one primitive; turns the silent lost update into an explicit,
  recoverable conflict; makes future upload retries safe (the LAW-285
  research's precondition).
- Costs: a wire-format change with back-compat on an old CLI; the restart
  loop needs the same "ambiguous outcome" care the buffered path already
  documents; concurrent restarts can livelock without a bounded retry.

**(c) Pointer-versioning tolerance in the check.** Change the gate's drift
verdict: a committed pointer whose named revision is still operative in the
live chain but is no longer the head (a **chain extension** by a later
writer) is reported as drift but as a warning-tier finding, not a
cd-blocking error; a pointer whose revision is absent, soft-deleted, or
byte-mismatched keeps failing. The client already holds every field needed
(`PointerVersion.version`/`asset_id`/`sha256`, `operative` flags).
- Touches: CLI only (`cli/src/projects/document_check.rs`), plus the
  `ci`-gate semantics in `append_document_check`. No store or portal change;
  no workflow change.
- Wins: cheapest to ship; directly removes the "PR that merged second fails
  cd on content it did not lose" symptom; no data model change.
- Costs: does not fix the underlying lost-update — the losing upload's bytes
  are still silently demoted; downgrading drift risks teaching the gate to
  ignore real drift if the warning tier is ever surfaced loosely. Needs a
  precise definition of "extension by a later writer" (head created after
  the pointer's `created_at`) to avoid masking genuine byte drift.

**(d) Upload idempotency key (companion, not standalone).** Client digest
(`project_id`, `slug`, `sha256`) on the buffered POST; the server treats a
repeat as `Filed::Unchanged`. Closes the same-bytes duplicate and unlocks
safe upload retries, but does nothing about the different-bytes tie-break.
- Touches: CLI + portal upload doors; pairs naturally with (b).

Recommendation: (c) first as the cd-facing relief (CLI-only, one release to
callers), with (b) designed as the durable fix — it reuses two patterns the
codebase already trusts (`stable_asset_id` election, `derived_from`-shaped
conditional validation) and makes (a)'s engine-level ambitions unnecessary
for this failure. (a) alone buys little that (b) does not, at higher risk.

## 6. Rollout: caller repos pin the gate by release tag

Three pins stand between any fix and callers, and all three lag:

- Callers pin the workflow itself to an exact release tag:
  `uses: neon-law/navigator/.github/workflows/project-gate.yml@YY.M.D` — the
  workflow "does not resolve `latest` and does not trust a caller-supplied
  version" (`.github/workflows/project-gate.yml:4-19`). `ops github setup`
  bumps the caller's pin; until each caller bumps, it runs the old gate.
- The self-referencing `navigator-install` action inside `project-gate.yml`
  "is pinned to an absolute tag, not to the ref the caller used, so they
  necessarily lag by one release" (`.github/workflows/project-gate.yml:80-89`;
  currently `@26.11.7`). A CLI fix shipped in release N is not visible to
  gate runs until the workflow's action pin bumps in a later release.
- The CLI version itself comes from the caller's `navigator.yaml`
  `version:` field (`read-manifest` job, lines 41-100), so even a CLI-side
  fix like (c) rides each caller's manifest bump.

Consequences: (c) — a pure CLI behavior change — still needs a release cut,
an action-pin bump, and caller manifest/pin updates before the symptom stops;
expect roughly a two-release tail, as the ENG-671 comment documents.
(b)/(d) — store and wire changes — are server-side, so they take effect when
the deployment rolls, independent of caller pins; old CLIs simply never send
the new expected-head field, so the server must treat its absence as "no
CAS" (the back-compat requirement from §5b). A workflow-YAML change is the
slowest to reach every caller and should be avoided for the fix itself.

## 7. Verification strategy for the recommended shape

For (c) (check tolerance) plus (b) (conditional append):

1. **Store-level race tests (the tie-break, today's behavior first).** Two
   concurrent `file_revision` calls on one `(project_id, slug)` — different
   bytes and identical bytes — asserting: today, both insert and head is
   UUIDv7-max, with the identical-bytes case producing two same-digest rows
   (documents the bug); after (b), the loser receives the 409 conflict and
   the chain holds exactly one operative head, with the identical-bytes
   restart resolving to `Filed::Unchanged`. Run under `tokio::join` with the
   Surreal test engine the store tests already use.
2. **Door-level back-compat tests.** `upload_document_door` (and the
   streaming complete door) with the expected-head field present vs absent:
   absent must behave exactly as today (no CAS), present must conflict on a
   stale head — pinning the old-CLI compatibility required by §6.
3. **Check-level verdict tests** in `cli/src/projects/document_check.rs`,
   which already mocks the integrity/revision doors: a committed pointer
   whose revision is operative-but-superseded by a newer head created after
   the pointer's `created_at` must produce the warning-tier finding and not
   reject `ci`; a pointer whose digest mismatches or whose revision is
   soft-deleted must still fail. Extend the existing `rejects(ci)` cases.
4. **Restart-loop test in the CLI upload path**: a mock door that returns
   409-with-current-head once, then accepts — asserting the restart re-reads,
   re-runs the Rule 3 dedupe, and converges in one round trip, and that a
   persistent conflict exits bounded rather than looping
   (the livelock bound from §5b).
5. **E2E on the real workflow** (kind-local-dev): two branches of a fixture
   Project repo, both editing the same slug; merge A, let A's author run
   `navigator project sync` against staging, merge B, and observe B's
   `documents` job — asserting B's cd is no longer red for the
   extension-only case while the real-drift cases stay red. Replay the
   sample-litigation signatures cited in the LAW-285 research as regression
   fixtures.

## Source trail

All file/line claims: inspected this session in
`/home/user/work/law284-research` (worktree of `cmp_wzsciwud`, branch
`task/research-law-284-documents-upload-race-r-qK3bfZNm`, base `origin/main`
at `3c89321`). LAW-285 prior art: `docs/research/
gate-documents-resilience-research.md` @ `ff6c892` (fetched from its branch
this session). Run-log citations (`41871492915`, `40941026632`, `42335752900`
et al.) are quoted from the LAW-285 document, not independently re-pulled.
Linear issues LAW-146/LAW-157 were not read directly; their content here
comes from the code comments and tests that cite them.
