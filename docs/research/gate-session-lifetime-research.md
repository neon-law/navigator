# LAW-268 research: the gate's `--check --ci` and the ten-minute CI session

Issue: `project gate --check --ci` outlives the 10-minute CI session on a Project with ~400 document
pointers.

**Evidence basis.** Every code claim below was inspected this session against `origin/main` at
`4610f065` (`feat(store,cli): conditional document appends with expected-head CAS and a bounded 409
restart (#533)`), which contains the merged LAW-285 work (PRs #522/#524) and the LAW-284 work (PRs
#531/#533). Line numbers are from that tree. Run-log evidence was not accessible this session: the
gate is a reusable workflow that runs in caller repositories, and the active caller repositories are
archived and low-traffic (the LAW-285 research doc, branch
`task/research-law-285-project-gate-yml-docume-seOXFW7c`, reached the same conclusion). The concrete
failure evidence is the commit message of PR #424, quoted in §4, plus worst-case arithmetic from the
code constants.

## Executive summary

1. **The 10-minute limit is not a GitHub job timeout.** The `documents` job sets no
   `timeout-minutes` (`project-gate.yml` has none anywhere; GitHub's default job timeout is 6 hours,
   which `remote.rs:683-684` itself calls "the job's six-hour runner budget"). The limit is the
   server-minted CI session: `CI_SESSION_TTL_SECS: i64 = 10 * 60` (`portal/src/session.rs:40`),
   applied to the document token at `portal/src/ci_auth.rs:181` and additionally capped at the
   runner's OIDC JWT `exp` (`portal/src/ci_auth.rs:255`), so the effective session can be *shorter*
   than ten minutes.
2. **The revision-read phase already survives session expiry** — PR #424 added the `LiveSession`
   renewal and concurrency-8 reads for exactly this failure. What has *no* renewal is the integrity
   paging phase that runs first (`document_check.rs:318` calls `client.integrity(deep)` on the raw
   client; `integrity_page` treats a 401 as terminal, `remote.rs:1516-1532`). A session that expires
   mid-paging fails the job outright.
3. **On a healthy deployment the check fits the session with a wide margin** (~20-90 s for 400
   pointers at the measured ~1.5 s/read from PR #424's era, ÷ 8 concurrent). The budget is at risk
   when per-request latency degrades: LAW-285's retries turn a slow deployment into up to ~4.2 h of
   worst-case revision reads (survivable only through repeated 401-triggered renewals, each costing
   a failed read plus a full re-mint) and ~40 min of worst-case integrity paging (not survivable —
   no renewal on that path).
4. **Recommended fix (read-only, two coordinated CLI changes):** route the integrity fetch through
   the same renewable session the revision reads use, and make the renewal proactive — re-mint
   shortly before the session's TTL instead of waiting for a 401. A follow-up second change removes
   the per-pointer read floor entirely: skip the revision read for any slug whose integrity row
   already proves the committed pointer (digest, size, version, operative — all present in
   `IntegrityAsset` today), adding `filename` to the integrity door so pointer-rename detection is
   preserved.

## §1 Where the `--ci` documents check spends its time

`navigator project gate --check --ci` runs `document_check::run` with `ci = true`
(`cli/src/projects/document_check.rs:119`). The workflow's `documents` job runs exactly
`navigator project gate --check --ci` (`project-gate.yml:131-132`); no `--deep`, so `deep = false`
in CI.

### The three remote phases, in order

**Phase 0 — mint (once, before any check work).** `resolve_ci_document`
(`cli/src/remote.rs:615`) fetches the runner's OIDC token (`actions_oidc_token`, `remote.rs:702`,
retried with `GATE_MINT_ATTEMPTS = 4` attempts / `GATE_MINT_RETRY_DELAY = 500 ms` × attempt /
`GATE_MINT_TIMEOUT = 30 s` per request, `remote.rs:680-685`) and mints the document token
(`mint_ci_token`, `remote.rs:776-836`, the same posture). Worst case ≈ 2 × (4 × 30 s + 3 s of
delays) ≈ **246 s**; healthy case ≈ 1-3 s.

**Phase A — integrity paging (sequential, once).** `check_with` calls
`client.integrity(deep).await?` directly on the raw client (`document_check.rs:318`) — *not*
through the renewable session. `integrity()` (`remote.rs:1463-1492`) walks pages of
`INTEGRITY_PAGE = 50` rows (`remote.rs:1038`) sequentially; 400 pointers = 8 pages (40 pages at the
deep page size of 10, `remote.rs:1040`). Each `integrity_page` (`remote.rs:1494-1545`) retries
`INTEGRITY_ATTEMPTS = 3` times with `INTEGRITY_RETRY_DELAY = 250 ms` × attempt and a
`INTEGRITY_REQUEST_TIMEOUT = 100 s` per request (`remote.rs:1044-1046`); only `transient_status`
(500/502-504, `remote.rs:693-695`) and `transient_transport` (connect/timeout, `remote.rs:697-699`)
retry. **A 401 is neither: it fails the page — and the whole job — on the first attempt, with no
renewal path.** Worst case ≈ 8 × (3 × 100 s + 0.75 s) ≈ **40 min**; healthy ≈ 8 × 0.3-1 s.

**Phase B — per-slug revision reads (concurrent, renewable).** `live_slugs(&integrity.assets)`
collects *every* live slug from the integrity rows (`document_check.rs:321`, helper at 442), then
`resolve_live_slugs` (`document_check.rs:577-631`) reads each slug's full revision chain via
`LiveSession::list_each` — a `futures::stream::iter(...).buffered(REVISION_CONCURRENCY)` with
`REVISION_CONCURRENCY = 8` (`document_check.rs:148`, applied at 285). That is **one remote call per
live slug** — `GET /app/api/projects/{id}/documents/revisions?slug=…`
(`remote.rs:1388-1440`) — regardless of whether the checkout has a pointer for it. Each read
carries the integrity posture: 3 attempts / 250 ms × attempt / 100 s per request
(`remote.rs:1393-1400`), and a 401 is a terminal `SessionRefused` *for the retry helper* but is
caught one level up: `LiveSession::list_revisions` (`document_check.rs:204-228`) renews the session
once through `remint` and retries the read; a second refusal marks the session exhausted and every
remaining slug fails as "CI session expired or is unauthenticated" (`document_check.rs:230-276`).

**Phase C — reconciliation (local only).** `reconcile_slug` (`document_check.rs:644`) and the
LAW-284 warning tier `chain_extension_warning` (`document_check.rs:750-778`) are pure local
functions over data already fetched — the committed pointer, the `RevisionsResponse`, and the
integrity rows. No remote calls happen after `resolve_live_slugs` returns.

### Per-call worst cases, from the constants alone

| Call | Attempts | Timeout | Backoff | Worst per call |
| --- | --- | --- | --- | --- |
| OIDC fetch (`remote.rs:724-730`) | 4 | 30 s | 500 ms × n | ≤ 123 s |
| CI mint (`remote.rs:784-824`) | 4 | 30 s | 500 ms × n | ≤ 123 s |
| Integrity page (`remote.rs:1505-1542`) | 3 | 100 s | 250 ms × n | ≤ 300.75 s |
| Revision read (`remote.rs:1393-1400`) | 3 | 100 s | 250 ms × n | ≤ 300.75 s |

### Wall-clock arithmetic for ~400 live slugs (shallow mode)

| Scenario | Phase 0 | Phase A (8 pages, seq.) | Phase B (400 reads, 8-wide ⇒ 50 waves) | Total |
| --- | --- | --- | --- | --- |
| Healthy deployment (~0.3 s/read, ~0.5 s/page) | ~2 s | ~4-8 s | 400 × 0.3 s ÷ 8 ≈ 15 s | **~20-30 s** |
| PR #424-era (1.5 s/read) | ~2 s | ~5 s | 600 s sequential — the documented failure; ÷ 8 ≈ 75 s | **~90 s** |
| Retry storm (one extra attempt per call) | ~6 s | ~8 s | 50 waves × ~3 s ≈ 300 s at 1.5 s/attempt | **~6 min** |
| Degraded (all attempts exhaust) | ~246 s | ≈ 40 min | ≈ 4.2 h | hours; phase B survives only via renewals |

The healthy-deployment numbers say the issue as titled — "*--check --ci* outlives the session at
~400 pointers" — does **not** reproduce on a fast deployment after PR #424: 400 reads at the
measured 1.5 s each, divided by the concurrency #424 introduced, is 75 s, three orders of magnitude
inside the session. What the constants *do* show is a cliff: at ~12 s of per-read service time the
healthy-path phase B alone (400 × 12 ÷ 8 = 600 s) reaches the session TTL on a single window, and
beyond it the run depends entirely on the renewal loop working. Slow pages are more dangerous than
slow reads, because phase A cannot renew at all (§2).

## §2 The 10-minute limit's actual source

- **Not a workflow timeout.** `project-gate.yml` contains no `timeout-minutes` on any job
  (`read-manifest`, `verify`, `documents`, `seeds`, `notify` all unset). GitHub's default job
  timeout is 360 minutes, and the codebase says so: `GATE_MINT_TIMEOUT`'s comment is "so a hung
  endpoint burns an attempt instead of the job's six-hour runner budget" (`remote.rs:683-685`).
- **It is the CI session TTL.** `portal/src/session.rs:36-40`:
  `pub const CI_SESSION_TTL_SECS: i64 = 10 * 60;` — "CI seed-token lifetime — 10 minutes. A seed
  reconcile needs far less than a laptop login, and a replayed GitHub OIDC JWT must not yield a
  long-lived Navigator session." The document token mints with the same constant:
  `let exp = now_unix_secs() + CI_SESSION_TTL_SECS` (`portal/src/ci_auth.rs:181`, the
  `POST /auth/ci/document-token` door at 85-95).
- **The effective ceiling can be lower than ten minutes.** `portal/src/ci_auth.rs:255` caps the
  minted session at the OIDC JWT's own `exp`:
  `let exp = (now_unix_secs() + CI_SESSION_TTL_SECS).min(claims.exp);`. GitHub's OIDC reference
  (docs.github.com/en/actions/reference/security/oidc, fetched this session) documents `iat`/`exp`
  on the runner JWT but states no fixed lifetime, so the exact ceiling is not documented upstream;
  the minted session is `min(600 s, remaining JWT validity)`. PR #424's commit message describes the
  same bound: "ten minutes at most, capped at the OIDC token's `exp`".
- **Which LAW-285 constants already risk the session even on a healthy deployment.** The mint-path
  constants (`GATE_MINT_*`, worst ≈ 246 s) are one-time costs, not per-pointer, and spend session
  time only *before* the session exists — harmless to the TTL. The risk lives in the shared
  read posture (`INTEGRITY_ATTEMPTS = 3`, 250 ms × n, 100 s timeout) applied per revision read:
  a deployment answering 5xx or timing out *some* of the time multiplies every affected slug's
  latency by up to 3× and adds up to 300 s of hang per slug. Retry storms do not consume the
  session server-side (the TTL is mint-time-fixed), but they stretch phase B past the TTL, which
  converts every expiry into a failed read + full re-mint (`LiveSession::renew`,
  `document_check.rs:230-263`) — latency on top of latency, and unrecoverable if the expiry lands
  during phase A, which has no renewal. The `min(claims.exp)` cap makes this more likely than the
  nominal ten minutes suggests whenever the runner's OIDC JWT is the shorter of the two.

## §3 What LAW-285 and LAW-284 changed and what each deferred

**LAW-285 (PRs #522/#524) — the retries and timeouts now in the tree.** Every gate remote call
gained the shared posture: `retry_gate_call` (`remote.rs:657-677`) with the mint constants
(4 attempts / 500 ms × n / 30 s, `remote.rs:680-685`) on the OIDC fetch and both mints, and the
integrity constants (3 attempts / 250 ms × n / 100 s, `remote.rs:1044-1046`) on integrity pages
*and* revision reads — `list_revisions` was the timeout-less, retry-less per-pointer call before
LAW-285; now it is bounded (`remote.rs:1393-1400`). Effect on this budget: single-attempt failures
became up-to-3-attempt latencies per pointer, so the worst case grew roughly 3× per call, and a
hung gateway now costs 100 s per attempt instead of hanging the job until GitHub's 6-hour default —
bounded, but 100 s × 3 × 50 waves is 4.2 h of theoretical phase B. **The LAW-285 spec
(`art_p3DBGVuR`) explicitly deferred this issue**: its scope boundary names "LAW-268 (gate session
lifetime)" among the out-of-scope items, alongside upload idempotency and page resume, and its
retries "only change *when a run gives up*, never *what it decides*".

**LAW-284 (PRs #531/#533) — verdict tier and upload CAS.** Two changes touch the check's surface:
`chain_extension_warning` (`document_check.rs:750`) reclassifies a valid chain extension from a
blocking drift verdict to a warning using data the check already holds — the committed pointer plus
the fetched `RevisionsResponse` — so it added no remote read and no budget cost (and avoided the
remedy of re-running reads the old error text implied). The 409 restart path
(`UPLOAD_RESTART_ATTEMPTS = 1`, `remote.rs:1273`) lives in the buffered-upload/`project sync` path
only: the CI documents check uploads nothing (`--ci` writes nothing), so it is budget-neutral here.
**The LAW-284 spec (`art_EAJU6PPl`) does not mention LAW-268 at all** — its scope notes confine the
gate to its read-only role and defer engine-level locking, the seeds job, portal UI, and any
workflow-YAML change; session lifetime was neither claimed nor deferred by name. What LAW-284 left
this issue is the structural observation its evidence table records: the check is point-in-time
inconsistent under concurrency and stays so by design — any LAW-268 fix must preserve that honesty
(reads stay read-only and meaningful), not paper over it with more reads.

## §4 Concrete failure evidence

- **PR #424's commit message** (`c3e5db99`, "fix(gate): renew the CI session and read revisions
  concurrently in the document check (#424)") is the one directly observed occurrence of the
  failure family, retrieved this session via `git show`:

  > "`navigator project gate --check --ci` mints one document session (ten minutes at most, capped
  > at the OIDC token's `exp`) and then read every pointer's revision chain sequentially at ~1.5 s
  > per request. On a Project with ~400 pointers the run outlived the session; the later reads
  > failed with `401 Unauthorized` and the same pointers were then reported as `no live document
  > for this pointer; run navigator project sync…`, which is wrong and sends the lawyer chasing
  > documents that are live."

  #424's fix is why the failure mode changed shape: sequential reads (400 × 1.5 s = 600 s ≈ the
  whole session) became concurrency-8 reads with one 401-triggered renewal, and a 401 can no longer
  read as a missing document (`is_session_failure`, `document_check.rs:163-168`).
- **No post-#424 run logs are accessible.** `gh run list` for the workflow returned nothing this
  session (the gate runs in caller repositories; the active callers are archived), so the remaining
  evidence is arithmetic (§1): the terminal session failures still reachable in the current tree
  are (a) a 401 landing during integrity paging — no renewal on that path
  (`document_check.rs:318` + `remote.rs:1516-1532`) — reachable when pages average ≥ ~75 s so eight
  pages cross the TTL, and (b) a renewal mint that fails or is refused, exhausting the session for
  every remaining slug (`document_check.rs:265-276`). Both produce a red `documents` job without a
  single pointer being wrong.

## §5 Viable fixes, priced

The invariant every fix must hold: the gate reads stay **read-only** (`--ci` writes nothing) and
**meaningful** — a green `documents` job must mean the checkout matched the live records as of the
run. None of the fixes below changes a verdict; they change only *when the run gives up* and *how
many reads it needs*.

1. **Route the integrity fetch through the renewable session** (CLI-only). Pass the `LiveSession`
   (or reuse its `renew` logic) around `client.integrity(deep)` so a 401 mid-paging renews and
   retries once, exactly as revision reads do today. *Point-in-time consistency:* unchanged — pages
   are already sequential snapshots; renewal does not re-read or weaken anything. *Risk:* lowest of
   the set; one code path gains the renewal idiom #424 already proved on phase B. *Tests:* extends
   the existing mocked-door families — the 401-renewal cases around `LiveSession` and
   `a_gateway_error_on_one_page_is_retried` and
   `a_page_that_keeps_failing_fails_with_bounded_diagnostics` (`document_check.rs:2401`, `2426`),
   plus a new "integrity 401 renews once, second 401 exhausts" case mirroring the
   session-exhaustion assertions.
2. **Proactive re-mint (session budget guard).** The check knows the session is minted at T=0 with
   a TTL of `min(600 s, OIDC exp)`; re-mint at ~80% of the TTL (or after N reads) instead of
   waiting for a 401. *Consistency:* unchanged — a renewal is not a re-read; each read still
   answers for itself. *Risk:* low; adds one mint per ~8 minutes of runtime, and a mint storm is
   impossible because the re-mint is serialized through the same `renewing` mutex
   (`document_check.rs:230-236`). A complementary variant fails fast with a named diagnostic when
   projected work (live-slug count × measured per-read latency) exceeds the remaining session,
   converting an opaque expiry cascade into an actionable message. *Tests:* unit tests against the
   mock server asserting a re-mint fires before any 401 when the clock crosses the threshold
   (the repo's mock-clock pattern in `remote.rs`'s tests, e.g. the paused-clock mint tests at
   `remote.rs:6111-6116`), plus the existing renewal tests unchanged.
3. **Per-pointer memoization keyed on the integrity snapshot (digest-based read skip).**
   `IntegrityAsset` already carries `sha256`, `size_bytes`, `version`, `operative`, and `exists`
   (`remote.rs:1050-1068`), and the committed pointer carries the same triple in
   `current_version: PointerVersion` (`store/src/document_pointers.rs:358`). A slug whose
   integrity row is operative and whose pointer matches on asset id + sha256 + size is *proven* by
   the snapshot itself; its revision read can be skipped. **One gap must be closed first:** the
   desired-pointer derivation needs the operative revision's `filename` (`RevisionSummary`,
   `remote.rs:1014-1024`) to detect renames, and `IntegrityAsset` has no filename — so the skip
   needs one field added to the integrity door, or it must read any slug whose pointer's on-disk
   name could have drifted, which is most of them. *Consistency:* the verdict for skipped slugs
   becomes "matches the integrity snapshot" rather than "matches a freshly read chain" — the same
   guarantee the integrity page already issues for storage presence and size, so the check gets
   marginally more consistent (one snapshot), not less, provided the filename field is included.
   *Risk:* moderate — a silent skip on a field the server populates lazily would hide drift; the
   door field addition must be a server-side contract test, and the skip must be
   conservative-by-default (skip only on a full quadruple match: asset id, sha256, size,
   filename). *Payoff:* 400 reads → reads only for missing, drifted, or non-operative slugs — the
   healthy-path phase B collapses toward zero and the session stops mattering at any realistic
   pointer count. *Tests:* extends `a_large_matter_is_fetched_in_bounded_pages_and_fully_checked`
   (`document_check.rs:2358`) with a matched-pointer-skips-the-read assertion, and the drift
   families (`a_live_document_with_no_pointer_fails_under_ci` 1707, `ci_reports_a_drifted_pointer…`
   1842, `a_pointer_whose_revision_the_chain_no_longer_holds_still_rejects` 1977,
   `a_superseded_pointer_with_mismatched_digest_still_rejects` 2004) asserting none of them is ever
   skipped.
4. **Raise `REVISION_CONCURRENCY`** (8 → 16/32). Linear speedup of phase B's healthy path; no
   consistency change (reads are independent; `buffered()` preserves order). *Risk:* deployment
   load — 8 concurrent per-slug chain reads is already the heaviest read fan-out the gate makes;
   doubling it on a small deployment is a self-inflicted slowdown. *Tests:* trivial (the constant),
   plus a bounded-in-flight assertion if the team wants the ceiling enforced.
5. **Batch or re-paginate the doors.** Two shapes: (a) a batched revision door
   (`?slugs=a,b,c…`) or (b) extending the integrity door to carry chain summaries (the LAW-284
   pointer triple already travels in `PointerVersion`). Both are server changes with
   back-compat requirements (old CLIs must see unchanged behavior when the field is absent, the
   same rule LAW-284 set for `expected_head`). *Consistency:* (b) is the most consistent option —
   the whole verdict rests on one snapshot — and it obsoletes phase B. *Risk:* highest surface of
   the set (wire format, server tests, the two-release caller-pin tail is irrelevant here because
   the check is shipped CLI-side but the *door* is server-side and lands at deployment roll).
   *Tests:* server-side door tests mirroring `portal/tests/ci_seed_token.rs`'s shape, plus the
   same mocked-door families as fix 3 on the client.
6. **Splitting the job** (chunk the check, one session per chunk). Each chunk mints its own
   10-minute session, so no chunk outlives it. *Consistency:* worse — chunks run at different
   times, so the verdict is no longer one point in time; and the split lives in
   `project-gate.yml`, the slowest channel to reach callers (the caller-pin analysis in the
   LAW-284 spec, research doc §6). *Risk:* workflow churn and a two-release rollout for a problem
   a CLI fix removes entirely. Not recommended.
7. **Relaxing per-call timeouts.** Counterproductive: the 100 s timeout is what converts a hung
   gateway into a retryable attempt (LAW-285's whole point). Raising it lengthens the worst case
   per slug proportionally and hides the budget problem without fixing any session path. Rejected.

## §6 Recommendation and test plan

**Recommended fix — ship fix 1 + fix 2 together in one CLI PR; fix 3 as the follow-up that makes
the session irrelevant.** Fix 1 (renewable integrity phase) removes the only terminal
session-expiry path the current tree has; fix 2 (proactive re-mint) removes the failed-read +
re-mint latency tax that a slow deployment pays on every expiry and bounds the run to one mint
per session window instead of one per expiry burst. Together they make ~400 pointers — and any
count — survivable on a degraded deployment at the cost of at most a handful of mints, with zero
verdict changes and zero server changes. Fix 3 (digest-based read skip + `filename` on the
integrity door) is the follow-up that removes the per-pointer read floor: after it, phase B costs
nothing on the healthy path, and the session TTL stops being a function of pointer count at all.
Splitting the job (6) and relaxing timeouts (7) are rejected for the reasons priced above.

**Test plan (all against the existing mocked-door families in `document_check.rs`):**

1. *Integrity 401 renews:* mock server returns 401 on the first integrity page request, then
   success; assert the check completes, exactly one re-mint fired, and the page set is fully
   checked. (New case beside `a_gateway_error_on_one_page_is_retried`, `document_check.rs:2401`.)
2. *Integrity exhaustion stays terminal:* 401 on every attempt including after renewal → the job
   fails with the session-exhausted wording, not a missing-document wording. (Mirrors the
   `LiveSession` exhaustion assertions.)
3. *Proactive re-mint:* with a paused/mock clock (pattern: `remote.rs:6111-6116`), cross the
   re-mint threshold mid-run and assert a single renewal fires *before* any read is refused, and
   that the read stream continues without a 401 anywhere.
4. *No verdict drift:* the full existing suite — drift, chain-extension warning, soft-deleted,
   superseded, absent-revision cases (`document_check.rs:1916-2120`) — passes unchanged, proving
   the fixes changed only when the run gives up.
5. *(Fix 3 follow-up)* `a_large_matter_is_fetched_in_bounded_pages_and_fully_checked`
   (`document_check.rs:2358`) extended with a matched-pointer-skips-the-read wiremock assertion,
   plus the assertion that no drift family above is ever skipped; server-side, a contract test
   that the integrity door's new `filename` field is present on every live row.

## Appendix: citation index (all inspected this session at `origin/main` `4610f065`)

| Claim | Source |
| --- | --- |
| Session TTL = 600 s | `portal/src/session.rs:36-40` |
| Document token mint + TTL, `min(claims.exp)` cap | `portal/src/ci_auth.rs:85-95, 181, 255` |
| No `timeout-minutes` in the workflow; check step | `.github/workflows/project-gate.yml:131-132` (whole file scanned) |
| "six-hour runner budget" comment | `cli/src/remote.rs:683-685` |
| Mint posture (4 / 500 ms / 30 s) | `cli/src/remote.rs:680-685, 724-730, 784-824` |
| Integrity posture (50 / 10 / 3 / 100 s / 250 ms) | `cli/src/remote.rs:1038-1046` |
| `retry_gate_call` backoff shape | `cli/src/remote.rs:657-677` |
| Transient classification (500/502-504, connect/timeout) | `cli/src/remote.rs:693-699` |
| Integrity paging loop, sequential | `cli/src/remote.rs:1463-1492` |
| `integrity_page` 401-terminal, no renewal | `cli/src/remote.rs:1494-1545` |
| `list_revisions` posture + 401 → `SessionRefused` | `cli/src/remote.rs:1388-1440` |
| Raw-client integrity call in the check | `cli/src/projects/document_check.rs:318` |
| `REVISION_CONCURRENCY = 8`, `buffered` | `cli/src/projects/document_check.rs:148, 285` |
| `LiveSession` renewal, exhaustion | `cli/src/projects/document_check.rs:163-276` |
| All live slugs read, one call each | `cli/src/projects/document_check.rs:321, 442-449, 577-631` |
| Chain-extension warning is local-only | `cli/src/projects/document_check.rs:750-778` |
| `IntegrityAsset` fields (no filename) | `cli/src/remote.rs:1050-1068` |
| `RevisionSummary` fields (filename, created_at) | `cli/src/remote.rs:1014-1035` |
| `PointerVersion` triple on the pointer | `store/src/document_pointers.rs:358` |
| PR #424 failure description and fix | commit `c3e5db99`, message quoted in §4 |
| LAW-285 defers LAW-268 by name | pinned spec `art_p3DBGVuR`, scope boundary |
| LAW-284 does not mention LAW-268 | pinned spec `art_EAJU6PPl` (out-of-scope list) |
| GitHub OIDC JWT: iat/exp only, lifetime undocumented | docs.github.com/en/actions/reference/security/oidc (fetched this session) |
