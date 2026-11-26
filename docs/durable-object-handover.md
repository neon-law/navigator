---
title: "Durable object handover"
description: "Transfer retained Notation machines and Project Slack posting evidence into PostgreSQL."
---

# Durable object handover

The Notation and Project Slack DBOS adapters require one activated PostgreSQL ownership epoch. Importing an offline
capture does not activate that epoch. PostgreSQL application authority, selected DBOS caller flags and a matching file
hash each serve a different purpose; none proves that retained object state has been transferred.

## Capture and check

A Deployment Operator holds the source's application and worker writers, including any caller that can create a Project,
through capture, import and activation. The acknowledgement flag records that operational responsibility. It does not
machine-prove a freeze, and matching SQL readbacks are not a consistent archive.

Capture uses the existing Restate CLI's explicitly selected authenticated environment. It reads the two service
identities, their complete retained state and retained invocation input evidence twice. Counts detect truncated results;
unknown keys, scoped objects, unsupported journal versions, changed readbacks and nonterminal invocations refuse the
capture. Paused work in either object service must be reconciled before proceeding.

A retained successful start must contain the matching input and its actual specification and initial-state writes.
Successful no-op starts do not ground the original `ephemeral` value. Missing or conflicting initializing evidence
refuses transfer rather than deriving that value from a missing business row. The canonical questionnaire and workflow
parsers validate each spec and current state. Both machines transfer in the same transaction.

Capture files contain private work product. Keep them in an approved private directory outside every Git repository. The
command creates a new file with mode `0600`, refuses overwrites, and prints only a digest and status. Input files must
also be private regular files. Do not attach captures or raw command output to issues or pull requests.

```bash
hudson ops restate object-handover capture \
  --environment ENVIRONMENT \
  --output /private/capture-directory/objects.json \
  --operator-writers-held

hudson ops restate object-handover import \
  --environment ENVIRONMENT \
  --capture /private/capture-directory/objects.json \
  --sha256 PRINTED_SHA256
```

## Operator database role

Applying or activating a handover requires the installation's privileged migration/operator connection. Supply the
existing `NAVIGATOR_POSTGRES_*` coordinates for `navigator_owner` through the approved private operator environment;
these commands do not obtain migration credentials or elevate a runtime connection automatically. The ordinary
`navigator_app` role can read global handover and ownership metadata but cannot insert, update, delete or truncate it.
Keep that restriction intact. Normal Project creation and posting receipts retain their separate runtime grants.
Application-history admission still applies to the operator target; a privileged login does not replace those proofs.

Import defaults to a read-only parity preview. Add `--apply --operator-writers-held` to copy the captured rows in one
transaction. A conflicting native value, missing non-ephemeral Notation parent or unfinished native Notation operation
refuses the entire transaction. Interruption leaves no partially committed handover; retry the same capture after the
transaction rolls back. Import never updates or deletes existing native state.

The component parity receipt contains capture version, identity, digest, row counts and commit status. Its digest binds
input and target values. It does not authenticate an independently supplied file or prove source quiescence. Preserve
the private source evidence and operator acceptance independently.

## Activate and preserve posting evidence

Activation requires a fresh acknowledgement, unexpired capture evidence, matching authenticated source readbacks and
exact coverage of the current PostgreSQL Project inventory. A creation fence prevents a new Project from slipping
between inventory verification and epoch commit. The command takes the same native object lane locks before comparing
every imported value. It refuses drift and admits both native object adapters under one epoch.

The command changes database ownership metadata only. It does not change caller flags, register a service or stop a
worker. Keep old writers held until native caller routing is verified and the old handler paths cannot admit new work.

```bash
hudson ops restate object-handover activate \
  --environment ENVIRONMENT \
  --capture /private/capture-directory/objects.json \
  --sha256 PRINTED_SHA256 \
  --operator-writers-held
```

An activated epoch accepts no different source transfer. Exact capture replay still compares current values; native
progress is never overwritten to make an old capture pass. Incident re-handover requires a separate recovery procedure.
New native Projects initialize posting evidence atomically under the established epoch. Fixed-ID seeds and raw imports
do not attest fresh workflow ownership.

[Project Slack Deliveries](glossary/project-slack-delivery.md) suppress notices already recorded in the source's
retained 200-event main-push ring. An absent or evicted key has no stronger guarantee than that bounded source contract.
The normal native workflow identity and Project lane govern newly admitted events.

Client-view posting evidence is separate from channel existence and the main-push ring. A retained successful
`client_project_view` invocation suppresses its notice. Missing or expired evidence remains unresolved and holds that
client-view event. A genuinely new native Project starts with verified not-posted evidence. The importer does not infer
that an existing Project's notice was never posted from the absence of a retained invocation.

The provider post keeps its existing named DBOS checkpoint and retry contract. A separate PG-only checkpoint records the
delivery after that provider checkpoint. A receipt-write failure therefore retries the receipt, reusing the provider
checkpoint. Slack's crash window between accepting a post and recording its provider checkpoint retains its existing
at-least-once limitation; this handover does not claim provider-level exactly-once delivery.

Before activating ownership or deploying the corresponding Restate binding omission, verify that the two object services
have no pending invocations in either staging or production, including paused or dead-lettered work. In-flight work must
drain or receive an explicit disposition and restart on DBOS with the same idempotency key. A selector, a PostgreSQL row
count, or a matching capture digest is not drain evidence. The code does not replay or purge retained work.

## Cut over without a capture

An operator who explicitly discards retained Restate state can establish DBOS ownership in an enrolled PostgreSQL
deployment. Stop legacy writers and keep them stopped through the DBOS-only rollout. The command checks authenticated
aggregate counts and refuses unfinished invocations or failed outcomes. It reads no retained object or journal body.

```bash
hudson ops restate object-handover activate-without-capture \
  --environment ENVIRONMENT --operator-writers-held --acknowledge-discard
```

Use the operator PostgreSQL connection described above. The transaction records the decision and gives existing Projects
unresolved client-view evidence. It holds main-push notices for those Projects because old event keys cannot be
deduplicated. New Projects use native delivery records. A historical Notation whose machine is missing cannot restart
from BEGIN. Existing PostgreSQL machines remain available. The command never replays Restate invocations.

Verify staging completes new DBOS work with the legacy route unavailable. After restarting the executor, verify the same
workflow identity and committed effects before changing production caller routing.

## Recovery acceptance

Object parity is one cutover requirement. Retained invocations, journals, timers, promises, locks, queued work and
uncertain external effects still require preservation and reconciliation. Other retained services are outside this
command's two-object scope. PostgreSQL restore, DBOS recovery and repository-filesystem restore evidence remain separate
acceptance checks. Keep the source recovery resources until those checks and the operator's retained-work review pass.

Tests use the same fixed-query source adapter with controlled synthetic responses, then actual PostgreSQL import and
DBOS execution with an inert Source handle. They do not capture, import, activate or retire a shared environment.
