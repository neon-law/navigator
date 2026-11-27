# Durable workflows

Navigator runs durable work in the DBOS executor hosted by the web application. PostgreSQL stores execution state,
checkpoints, and ownership. See [DBOS durability](dbos-durability.md) for database roles and recovery across replicas.

## Where the schedule lives

The executor starts schedules from UTC slots. A scheduled workflow uses a deterministic ID derived from its service and
slot, so another replica or restart can join the same execution. A manual run uses a fresh `manual-<uuid>` key.

Inbound events and authorized manual actions submit to the authenticated DBOS ingress. The bearer token protects that
internal hop. The application checks the caller’s authority before it submits work.

## Idempotency is the workflow key

Start each durable job under a stable ID for its domain operation. Notation transitions serialize on the Notation.
Project Slack notices reuse IDs derived from the Project and event. Workflows checkpoint completed steps and use
delivery receipts where an external effect can repeat.

## Recovering an EmailSummary run

`hudson ops email-summary redrive --receipt <receipt-id> --recovery-id <operation-id>` uses the same
`EmailSummaryConfig::request_for` input as intake. It reuses the archived receipt, Letter, Project, channel, provider
configuration, and digest. It creates no second receipt or archive. Diagnostics contain status words only.

Retain the recovery UUID and repeat it after a lost response. The authenticated worker binds it to the exact request,
operator assertion, recorded source stages, version, and actual target. A retry joins that operation; changing its
receipt, input, or operator assertion is refused. The command reports that recovery was queued, not that the summary has
finished.

A receipt row lock admits one recovery owner and records any authorized failure-notice reopening in the same
transaction. Another operation can take ownership only after the previous controller and its actual target have a
verified terminal outcome, or after an explicit retry of a parked target. `--from-recovery <previous-operation-id>`
names that previous repair; it never guesses a source from timestamp order.

A completed provider-failure result retries the recorded provider stage. Reopening its confirmed failure notice also
requires `--after-failed-result <actor>`, recorded in the delivery audit. A failed delivery with a successful provider
checkpoint retries delivery without repeating that provider stage. An interrupted confirmed delivery stays confirmed. A
recorded unknown delivery requires the existing audited authorize-resend operation before retry; an outcome still
`unknown` or `sending` remains quarantined.

Unhandled workflow errors retry their recorded failed stage. Explicit recovery of canceled or parked work resumes its
existing checkpoints. The controller verifies compatible version, typed input, stage identity, and fork origin; it never
deletes workflow history or starts again from an arbitrary step. DBOS recovery can repeat a step that performed an
effect before losing its checkpoint, so completed checkpoints and native delivery receipts remain separate controls. See
[executor ownership](dbos-durability.md#several-replicas) before recovering work from a replaced pod.

## Adding a workflow

1. Author a Notation template workflow or register a DBOS workflow in its owning crate. See
   [Notation authoring](notation-authoring.md).
2. Give the job a stable key, bounded retries, and a defined schedule or authenticated caller.
3. Test restart recovery and deduplication at the boundary where the workflow makes its effect.

## Inspecting a workflow

Check the executor readiness probe and the application logs for the workflow’s submission outcome. Use the workflow ID
and application version to inspect its row in `dbos.workflow_status`. Check the appropriate delivery receipt to confirm
an outbound effect. Keep client content out of operational logs.

## Summary model contract

The request sends a `responseSchema` built by `workflows::summary_response_schema`, whose required properties equal the
serialized fields of `EmailSummary`, and the prompt names the same four keys. A candidate whose `finishReason` is not
`STOP` is never parsed. Its provider status begins `output_truncated:finish_reason=` and ends with the reason, the
output token count, and the thinking token count. An object the parser rejects begins `invalid_output:` and ends with
the same reason and count. Statuses carry reasons and counts only. The output budget defaults to 4,096 tokens because
thinking tokens share it.
