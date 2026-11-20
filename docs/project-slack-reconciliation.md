# Resolve retained client-view posting evidence

An unresolved client-view event remains blocked before native workflow creation. A channel's existence does not prove
that its notice was posted. A privileged operator can record an independently verified decision without sending a Slack
request or altering captured Source evidence.

Use `hudson ops project-slack reconcile-client-view` with these required arguments:

- `--operation`: a new audit UUID; retain it for an identical retry after a lost response.
- `--project`: the Project whose retained client-view event is unresolved.
- `--ownership-epoch`: its expected current active native ownership epoch.
- `--actor-person`: the existing Person attributed with the operator decision.
- `--evidence-reference`: an opaque UUID pointing to the private evidence the operator reviewed.
- `--decision posted` or `--decision verified-not-posted`: the explicit verified conclusion.

Use the [operator database connection](durable-object-handover.md#operator-database-role) for this command as well as
handover writes. Supplying a Person UUID does not log in as that Person. The store independently checks the attributed
Person is admitted and has the existing Firm-management permission: Owner, or Admin with an admin membership in the
Project's active Firm. Keep the underlying evidence in its approved private store; the command neither retrieves nor
verifies the evidence pointer's contents.

`posted` records a posted-event receipt so native replay skips that notice. `verified-not-posted` records no posted
receipt and permits the ordinary native workflow to post later. Neither decision starts a workflow. Both append an
immutable actor, evidence reference and decision audit in the same transaction as the disposition change. The Project's
native serialization lane prevents a concurrent post from crossing that decision.

The command refuses an inactive or changed epoch, missing authority, existing resolved evidence and conflicting audit
operations. An identical operation is idempotent; it cannot change the actor, evidence, Project, epoch or decision. It
never deletes native delivery receipts or overwrites a posted effect. Reconciliation resolves only this retained event;
it does not certify a writer freeze, full object handover, other retained workflows or provider recovery acceptance.
