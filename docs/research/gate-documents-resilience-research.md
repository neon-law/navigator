# LAW-285 research: resilience of the documents/notify deploy path in `project-gate.yml`

Read-only research against `origin/main` at `e4eca10` (`39d2b0b` is an ancestor).
No code changed. Every claim below points at a file and line inspected this
session unless marked otherwise.

## What the deploy path actually runs

`project-gate.yml` is a reusable workflow; each Project repository's `ci.yml`
calls it pinned to an exact release tag
(`.github/workflows/project-gate.yml:1-20`). Three feeder jobs make remote
calls and one posts a Slack notice:

| Job | Command | Remote behavior |
|---|---|---|
| `verify` | `navigator project gate --ci` | live Project-row check (main/PR merge refs only) |
| `documents` | `navigator project gate --check --ci` | live document check: OIDC, integrity pages, revision reads |
| `seeds` | `navigator site import [--dry-run] --ci` | seed reconciliation behind its own `/auth/ci/seed-token` door |
| `notify` | inline `curl`+`jq` (GitHub); `notify-main-push --ci` (Forgejo) | OIDC, mint, Project lookup, event post |

Key structural fact: **the `documents` job performs no upload.** `--ci`
"writes nothing" (`cli/src/projects/document_check.rs:107-116`); it only
compares committed pointers against the live deployment. The upload commands
(`navigator documents upload`, `document_upload_directory` —
`cli/src/remote.rs:2243, 2396`) are not invoked by this workflow. Publication
is now a no-op too: `project-publish.yml` exists only so existing `cd.yml`
files keep passing (`.github/workflows/project-publish.yml:26-46`). So
"resilience of the documents deploy path" means: **a transient failure in any
read or mint fails `cd` (via the required `ci` job) even though nothing was
being changed.**

## Remote-call inventory and current failure behavior

### 1. DNS preflight (CLI, before any request)

`preflight_dns` (`cli/src/remote.rs:733`) resolves the host with a blocking
`to_socket_addrs` before minting. A DNS miss prints a `::error::` annotation
naming the host and exits via the `MintRefusal` path — exit code `3`
(`exit_code_for`, `cli/src/remote.rs:854-865`). No retry, by design (a
fast-fail diagnostic, not a resilience gap). A non-DNS lookup error is left
for the real request.

### 2. OIDC token fetch — no retry, no timeout

`actions_oidc_token` (`cli/src/remote.rs:663-697`) reads
`ACTIONS_ID_TOKEN_REQUEST_URL`/`_TOKEN` and GETs the runner's OIDC endpoint
with **`reqwest::Client::new()` — no connect/total timeout and no retry**:

- transport error → `anyhow!("request {runner} OIDC token")`, job fails;
- non-2xx → `{runner} OIDC token request failed: {status}` and fails.

GitHub's token endpoint is normally reliable, but a runner-side network drop
here kills the job on the first request of the run. No `attempt`/backoff
exists anywhere in this function. The `notify` job's inline-curl variant has
the same shape: `curl -sSf` against `ACTIONS_ID_TOKEN_REQUEST_URL`
(`.github/workflows/project-gate.yml:210-212`), no retry flags.

### 3. Session mint `POST /auth/ci/document-token` — no retry

`mint_ci_token` (`cli/src/remote.rs:713-760`): one POST on the same
timeout-less client.

- DNS failure → preflight annotation + `MintRefusal` (exit 3);
- non-2xx → annotation `::error::CI token mint at /auth/ci/document-token was
  refused: {status}: {message}` + `MintRefusal` (exit 3);
- transport error (connect reset, timeout) → `unreachable_host_error`
  context; annotated only if DNS-shaped.

A transient 502/503/504 from the deployment (or a proxy in front of it) is
**terminal on the first attempt**. Observed signature (run `41871492915` of
`neon-law/sample-litigation`, 2026-11-04):

```
##[error]CI token mint at /auth/ci/document-token was refused: 403 Forbidden:
this GitHub Actions run is not bound to a live project
navigator: CI token mint at /auth/ci/document-token failed: 403 Forbidden: ... : CI token mint was refused
##[error]Process completed with exit code 3.
```

That instance is a deterministic refusal (the repo was being archived), but it
shows the single-attempt mint path: the job red-fails with nothing retried.

### 4. Project lookup `GET /app/api/projects` — the one well-defended call

`fetch_project_list_with` (`cli/src/remote.rs:5015-5052`): 4 attempts,
`500ms × attempt` growing delay, retrying 502/503/504, connect errors, and
timeouts; 401 maps to `UploadSessionExpired`; anything else returns at once.
Comment at `remote.rs:4968-4972` records the rationale (idempotent read).
This is the model LAW-285 wants the other calls to follow.

### 5. Integrity pages `GET .../documents/integrity` — partial defense

`integrity_page` (`cli/src/remote.rs:1273-1325`): 3 attempts
(`INTEGRITY_ATTEMPTS`, `remote.rs:923`), 100 s per-request timeout
(`INTEGRITY_REQUEST_TIMEOUT`, `remote.rs:924`), `250ms × attempt` delay
(`INTEGRITY_RETRY_DELAY`, `remote.rs:925`). Retries: 502–504, request
timeouts, connect errors. **Not retried: plain 500s and 4xx.** The caller
`integrity()` (`remote.rs:1242-1271`) pages with `INTEGRITY_PAGE = 50` rows
(`remote.rs:919`; `INTEGRITY_DEEP_PAGE = 10`, `remote.rs:921`) and aborts the
whole check if a page exhausts its attempts — a mid-page failure discards all
previously merged pages (no resume from `offset`, the run just fails).

### 6. Per-slug revision reads `GET .../documents/revisions?slug=` — no retry

`DocumentClient::list_revisions` (`cli/src/remote.rs:1194-1216`): single
attempt on a client with **no request timeout**
(`DocumentClient::with_credential` passes `None` — `remote.rs:1140-1160` —
and `authenticated_client_builder` sets none, `remote.rs:1101-1127`). A hung
gateway leaves the job hanging until GitHub's job-level limit. Failure
handling in `LiveSession` (`cli/src/projects/document_check.rs:184-288`):

- `401` → one session renewal through `remint` (a full re-mint: DNS preflight
  → OIDC fetch → `document-token` POST, `document_check.rs:119-127`), then
  one retry of the read; a second refusal marks the session exhausted and
  every remaining slug fails as "CI session expired or is unauthenticated".
- Any other error (5xx included) → pushed as a `CheckFailure` for that slug
  (`resolve_live_slugs`, `document_check.rs:567-600`) and the check fails.
  There is no read retry — one transient 503 on one slug of N fails the job.
  Reads run at concurrency 8 (`REVISION_CONCURRENCY`,
  `document_check.rs:129`), so a deployment hiccup can fail many slugs at
  once.

### 7. The notify job — no retry anywhere, but deliberately non-blocking

GitHub path (`.github/workflows/project-gate.yml:193-243`): four `curl -sSf`
calls — OIDC fetch, mint, `GET /app/api/projects`, `POST
.../events/main-push`. `-sSf` fails on any transport error or non-2xx; no
`--retry`. Mitigation: the job carries `continue-on-error: true` and `ci`
does not need it, so a Slack outage or refused mint does not fail `cd` —
but the notice is then silently lost (a lost Slack notice for a failed gate
is exactly the notification you wanted).

Forgejo path runs the same calls in-process
(`cli/src/projects/main_push.rs:92-133`): `reqwest::Client::new()`, no retry,
`error_for_status` on both reads — same single-attempt behavior, same
non-blocking position.

### 8. The upload path (not in CI today, for completeness)

`upload_bytes_buffered` (`cli/src/remote.rs:1590-1667`): one POST, no retry.
Its error contexts are deliberately phrased "upload outcome unknown; reconcile
with `navigator project gate --check` before retrying" — the CLI's own
admission that a transport failure leaves the outcome ambiguous. There is **no
idempotency key**: re-running may file a second identical revision.
`upload_bytes_streaming` (`remote.rs:1669-1806`) is resumable by construction
(session id + acknowledged offset, 409-driven resync bounded at 4, GONE =
session expired with progress lost, `remote.rs:1706-1806`) but a completion
ambiguity still says "upload may be filed".

The batch loop `document_upload_directory_from` (`remote.rs:2424-2580`)
preserves progress: per-file outcomes, digest-based duplicate skipping,
"unfiled: same bytes as unsuccessful …; reconcile before retrying" guard
against double-filing an ambiguous file, one login-renewal pause — and ends
"{failed} file(s) remain unfiled; successful uploads are preserved".

## What a failed run leaves behind

- `documents` job: nothing server-side (reads only, `--ci` writes nothing) and
  nothing client-side (the checkout is discarded). A failed run is clean; the
  cost is purely a red `cd` and a re-run from zero.
- `seeds` job (adjacent): `navigator site import` without `--overwrite` is
  idempotent by natural key (workflow comment,
  `project-gate.yml:159-167`), so a re-run is a no-op for already-imported
  rows — but a mid-import transient failure can leave a **partially imported
  seed set** with no resumable marker beyond the natural key itself.
- Uploads (manual path): ambiguous-outcome uploads can leave an orphaned
  revision (filed server-side, pointer never updated client-side) — the
  reconcile-with-`--check` guidance exists for exactly this. No orphaned
  pointers are created by a gate failure because the gate never uploads.

## Failure signatures in recent run logs

The reusable gate runs in caller repositories, so the searchable logs are the
callers'. What was visible from this environment (GitHub org listing shows no
active Project repositories beyond the archived `sample-*` trio; run lists
pulled with `gh run list` this session):

- `neon-law/sample-litigation` run `41871492915` (2026-11-04): `documents` and
  `verify` both failed with the exit-3 mint refusal quoted in §3 — the
  deterministic "not bound to a live project" 403.
- `neon-law/sample-litigation` run `40941026632` (2026-10-29): `documents`
  minted, found 0 document errors, then failed on the live Project
  coordinates check (`live Project coordinates check is inconclusive or
  disagrees: motion-for-summary-judgment.pdf: linked Docket Entry naming …`) —
  a real drift finding, not a transport failure.
- `neon-law/navigator` main `deploy` runs `42335752900`, `42332904682`,
  `42339817411` (2026-11-07) all failed in the KIND integration e2e job on
  classic transient infra: `ErrImagePull`/`ImagePullBackOff` on trigger
  images, `connection refused` to the Restate admin API and the
  `navigator-web` startup probe. Not the gate, but evidence the deploy
  surface's transient-failure rate is real — and of the contrast inside the
  same logs: the toolchain install curls retry (`--retry 10
  --retry-connrefused`) while the gate's remote calls do not.

**No log captured a transient 5xx or OIDC timeout hitting the gate itself** —
the accessible callers have too little traffic. The case for LAW-285 rests on
the code gaps (§2, §3, §6: three single-attempt calls, two of them on a
timeout-less client) rather than on a captured incident.

## Retry/timeout posture, summarized

| Call | Timeout | Retry | Delay | Idempotency |
|---|---|---|---|---|
| DNS preflight | blocking syscall | none (fast-fail by design) | — | n/a |
| OIDC token fetch | **none** (`Client::new()`) | none | — | read |
| Mint `document-token`/`seed-token` | **none** | none | — | n/a (single-use mint is fine to repeat) |
| `GET /app/api/projects` | **none** (client default) | 4× | 500 ms × n | idempotent read |
| Integrity pages | 100 s/request | 3× (502-504, timeout, connect) | 250 ms × n | idempotent read; no page resume |
| Revision reads (×8 concurrent) | **none** | 401-renewal only | — | idempotent read |
| notify curls / CLI post | **none** | none | — | last-write event; `continue-on-error` |
| Buffered upload | **none** | none | — | none; ambiguous outcome |
| Streaming upload | **none** | resumable session | — | session id + offset |

## LAW-284 context (not solved here)

Concurrent callers of the same deployment interact through two seams. First,
two runs' revision reads and integrity pages see the deployment at different
instants, so a PR's `--check` can disagree with a just-landed main push. Second,
the manual batch upload races itself on slugs: two directories uploading the
same slug both pass the in-process `slugs` set and file competing revisions of
one chain (the server-side chain tie-break is what LAW-284 targets; the CLI
detects only same-run collisions — `remote.rs:2478-2484` — and the gate's
`--check` reports cross-run drift as pointer disagreements). Any retry added
under LAW-285 should be safe against this: the existing reads are idempotent,
and a retried *mint* mints a second session, which the `LiveSession` renewal
machinery already tolerates (generation counter, `document_check.rs:192-205`).

## Where the leverage is

1. Give the OIDC fetch and both mint calls the `fetch_project_list_with`
   treatment (attempts, growing delay, retry on connect/timeout/502-504). All
   three are safe to repeat: the fetch and mint are read-shaped, and a second
   session mint is harmless.
2. Put a request timeout on the default `DocumentClient`/mint clients (or
   reuse `for_validation`'s 10 s where the verb is a read).
3. Retry the revision reads the way integrity pages already do — per-slug,
   2-3 attempts — so one 503 among N slugs fails nothing.
4. Leave `preflight_dns` as-is (diagnostic, not resilience) and leave notify's
   `continue-on-error` posture; optionally add `--retry 3
   --retry-all-errors` to the GitHub-path curls or accept the lost notice.
5. Buffered uploads need an idempotency story (client digest key) before any
   retry is safe — currently safe only because nothing in CI uploads.
