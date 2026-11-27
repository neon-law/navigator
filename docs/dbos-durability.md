# DBOS durability

Navigator runs [durable execution](glossary/durable-execution.md) in the DBOS executor hosted by the web application.
PostgreSQL stores workflow state and checkpoints. See [durable workflows](durable-workflows.md) for job ownership and
ingress.

## The pin

| Item | Value |
| --- | --- |
| Crate | `dbos` 0.6.0 (MIT), the official native Rust SDK |
| Database driver | SQLx 0.9 with ring rustls, the same build as the workspace |
| PostgreSQL | the pinned `navigator/postgres` image ([PostgreSQL compatibility](postgres-compatibility.md)) |
| Missing from 0.6.0 | a transactional step |

## Upgrading the deployed DBOS schema

The first deployment of DBOS 0.6 requires a coordinated cutover for each database. Quiesce workflow starts and let the
0.5 runs finish before changing the release tag, which changes the DBOS application version. Stop every 0.5 executor and
client sharing the system database before starting 0.6. The first 0.6 executor migrates the schema on launch. Start any
0.6 clients after the migration. Resume workflow starts after the new executor is healthy. Use a forward fix after
migration. Redeploying a release built with 0.5 cannot roll back the schema.

If a 0.5 run must be recovered after migration, 0.6 needs the old application version and executor identity to recover
it in place. See the [upstream upgrade instructions][dbos-upgrade] and the controlled handoff below before changing
either value.

[dbos-upgrade]: <https://github.com/dbos-inc/dbos-transact-rust/tree/v0.6.0#upgrading-from-05>

## Where DBOS state lives

- **Database:** the configured application database on `navigator-postgres`. Fresh native tiers use the database
  recorded in their creation intent; their DBOS connection targets that same database.
- **Schema:** `dbos`. The SDK creates and migrates its own tables there on launch (`Config.migrate`).
- **Login:** `navigator_dbos`, owning only the `dbos` schema. It has no rights on `public`. Application logins have no
  rights on `dbos`. It also holds `CREATE` on the database: the SDK runs `CREATE SCHEMA IF NOT EXISTS dbos` on every
  launch, and PostgreSQL checks that right before it checks whether the schema exists.
- **Who may write it:** only the SDK. Operators read it to debug and never edit its rows by hand.

The roles Job converges the login once its Secret exists ([PostgreSQL on Kubernetes](postgres-kubernetes.md)).

## The gap between an effect and its checkpoint

A DBOS step runs its body and then commits a checkpoint row. These are two commits. A crash between them means recovery
runs the body again. Every step is therefore **at least once**.

The rule that closes the gap:

1. Every step with an effect derives an [Effect Key](glossary/effect-key.md) from its position:
   `{workflow_id}/{step_id}`. Both are deterministic on replay. `workflows::dbos_engine::effect_key()` returns it inside
   a step.
2. **A PostgreSQL write** stores the effect key in a unique column, in the same transaction as the write. Use
   `INSERT … ON CONFLICT (effect_key) DO NOTHING` or an upsert, then read the stored row back so a re-run returns the
   first result.
3. **A provider call that accepts an idempotency key** receives the effect key. 4. **A provider call without one** stays
at least once. Name that in the workflow's docs, and keep the step to the
   single call so the duplicate window is only the crash between the provider's reply and the checkpoint.

`workflows-service/tests/dbos/effects.rs` proves both halves against PostgreSQL. An unkeyed effect repeats after a crash
inside that window. A keyed write lands once.

## Lifecycle in web

The web process starts DBOS and refuses readiness if the executor cannot start. The application drains in-flight HTTP
requests before it stops the embedded executor.

Configure `NAVIGATOR_POSTGRES_DBOS_PASSWORD` in the PostgreSQL secrets and apply the `navigator-postgres-dbos` Secret.
The release roles step converges the dedicated login and schema before the web rollout. Give the application and
authorized callers the same `NAVIGATOR_DBOS_INGRESS_TOKEN`. Kubernetes routes internal callers to the web Service on
port 9083.

### Several replicas

Every DBOS host must set `NAVIGATOR_DBOS_EXECUTOR_ID` to a unique value for each concurrently running pod. The web
manifest uses the pod name from Kubernetes. DBOS recovery uses both this identity and the application version: launch
re-enqueues pending runs assigned to this identity and version. A new pod therefore cannot re-enqueue a run a live peer
owns. Slot-keyed workflow ids still make duplicate schedule starts join the same run. The real-PostgreSQL executor tests
prove that a live peer leaves the held run pending and that the matching identity recovers it after shutdown.

The pod name survives a container restart in place, so that restart recovers its own pending runs. If Kubernetes
replaces the pod, the new pod name cannot recover the old identity's runs. To recover workflows with external effects,
operators must preserve the old identity from the pending row and perform a controlled handoff: stop all DBOS replicas,
launch one replica with `NAVIGATOR_DBOS_EXECUTOR_ID` set to the recorded old identity and the same release, wait for its
pending work to finish, then restore pod-name identity and scale the fleet back up. The SDK exposes no stale-executor
discovery or automatic replacement-pod recovery; do not edit DBOS tables. Pending ownership can be inspected read-only
with the deployment's approved database operator access:

```sql
SELECT workflow_uuid, executor_id, application_version
FROM dbos.workflow_status
WHERE application_name = 'navigator-workflows' AND status = 'PENDING'
ORDER BY executor_id, application_version, workflow_uuid;
```

Before a controlled handoff, record the `workflow_uuid` values for the abandoned executor and release as well. After
starting one replica with that identity and release, wait until each recorded workflow is terminal before restoring
pod-name identity or scaling the fleet up.

`workflows-service/tests/dbos/executors.rs` also proves recovery remains isolated by application version. Until
replacement-pod recovery is automated, deployments must account for this operator handoff before treating DBOS as a
fully unattended host for external effects.

The PostgreSQL persistence workflow rehearses replacement-pod recovery in a disposable namespace. It observes the
pending workflow, its application version, its executor identity, and the completed checkpoint. It waits for the first
pod to disappear before the replacement takes that identity. The replacement retrieves the existing workflow. The
rehearsal compares the journal and the synthetic effect receipt after recovery.

## ProjectRecovery

The DBOS schedule uses the verified [Project recovery archive](project-recovery-archives.md) accessors. Durable
checkpoints track each Project's preparation and publication.

## ProfitDigest

The deployment's configured finance provider and Slack channel govern delivery. The workflow checkpoints the business
query and notification separately.

## Linear backlog

The Linear backlog digest reads open work and posts to its configured Slack channel.

## Project Calendar membership

DBOS reconciles Project Calendar membership every six hours. Calendar and booking provisioning run on their own cadence
and retain domain-level idempotency.

## Native deployment browser gate

The deployment integration job provisions KIND through `hudson quickstart` and verifies PostgreSQL admission and DBOS
checkpoint replay across a restart. Browser suites exercise the exact built application image.
