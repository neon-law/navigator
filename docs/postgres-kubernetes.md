# PostgreSQL on Kubernetes

These overlays run Navigator's PostgreSQL database beside SurrealDB and Restate. The release workflow applies the
PostgreSQL server, client roles, and Navigator migrations before Forgejo or Navigator workloads. `web` connects to
PostgreSQL only in a deployment that opts in. A deployment without PostgreSQL renders and boots as before.

| Overlay | Applies | When |
| --- | --- | --- |
| `examples/deploy/k8s/gke/postgres/<env>` | PostgreSQL, archive, backups (100 GiB) | Before app and Forgejo |
| `examples/deploy/k8s/gke/postgres/access/<env>` | Navigator client roles | After server readiness |
| `examples/deploy/k8s/gke/postgres/migrate/<env>` | `navigator-migrate` Job | After roles, before workloads |
| `examples/deploy/k8s/gke/postgres/mirror/<env>` | `navigator-postgres-mirror-reconcile` Job | On demand |
| `examples/deploy/k8s/gke/postgres/restore-drill/<env>` | Point-in-time restore drill Job | On demand |
| `k8s/overlays/kind-postgres-persistence` | Server only (1 GiB) | Synthetic persistence and TLS tests |
| `k8s/overlays/kind/forgejo` | Server, roles, Forgejo, runner | [`dev forgejo up`](forgejo-kind.md) |

Production renders the same objects as staging, data-claim size included; a manifest test holds the two equal. Render
any overlay with `kubectl kustomize <overlay>`.

## Server and storage

The shared StatefulSet consumes the published compatibility image digest. GKE uses a dedicated regional balanced PD CSI
StorageClass with delayed binding, expansion, and `Retain` reclaim policy. Both claim retention policies are `Retain`.
Pod replacement, scaling to zero, and StatefulSet recreation reuse `data-navigator-postgres-0`. Deleting that PVC
releases its disk; reattachment requires a deliberate recovery procedure. Never delete the namespace or PVC as part of a
rollout. Shrinking is unsupported.

Every hosted row runs a 100 GiB data claim. The size has one definition, the base `volumeClaimTemplates` in
`k8s/postgres/postgres.yaml`; the GKE overlays add only the StorageClass, and the KIND overlays patch it down to 1 GiB.

A StatefulSet's claim template is immutable, so `kubectl apply` cannot grow it on a live row. `navigator ops ship`
handles that in its PostgreSQL step, before the apply. It derives each claim name from the rendered template
(`data-navigator-postgres-0`), compares the live template and claim sizes with the rendered one, and prints each action:

1. `kubectl patch pvc data-navigator-postgres-0` raises a smaller claim's request to 100 GiB. The CSI driver grows the
   disk and filesystem online; the pod keeps running.
2. When the live template is smaller, `kubectl delete statefulset navigator-postgres --cascade=orphan` removes only the
   StatefulSet object. The pod and claims stay.
3. The apply recreates the StatefulSet with the new template, and the controller adopts the running pod and its claim.

A converged row prints `nothing to grow`. A live template larger than the rendered one stops the release. `--dry-run`
prints the same plan and skips the PostgreSQL diff when a recreate is due, because the server-side diff would refuse the
template change. Confirm the result with `kubectl get pvc data-navigator-postgres-0`: `CAPACITY` reads `100Gi` once the
resize finishes.

The runtime is non-root with a read-only root filesystem, data/socket/temp volumes, resource limits, probes, and a
120-second shutdown grace period. The image uses SIGINT for fast shutdown and checkpointing. Avoid force deletion; wait
for clean disk detach.

This is one primary. Regional disk replication does not supply database replication or automatic PostgreSQL failover.
The retained PVC protects against pod rescheduling, not against disk loss or corruption; backups below cover that.

## Access: TLS, roles, and secrets

TCP access requires TLS and SCRAM. Local administration uses socket peer authentication. Only namespace-local pods
labeled `navigator.neonlaw.org/postgres-client: "true"` may connect. Every client uses `sslmode=verify-full` against the
namespace-qualified Service name, which the `navigator-postgres-client` ConfigMap carries as `host`; the server
certificate's SAN must include `navigator-postgres.<namespace>.svc.cluster.local`.

The `navigator-postgres-roles` Job converges the login roles in `k8s/postgres/access/roles.sql`. It is the only workload
that signs in as the bootstrap superuser.

| Login | Used by | May |
| --- | --- | --- |
| `navigator_owner` | `navigator-migrate` Job | Own the `navigator` database and `public` schema; run DDL |
| `navigator_app` | `web`, mirror Job | Connect; `SELECT`, `INSERT`, `UPDATE`, `DELETE` rows and use sequences |
| `navigator_backup` | WAL archive, base backups | `REPLICATION` only; no database privileges |
| `navigator_dbos` | `web` with DBOS on | Own `dbos`; database `CONNECT` and `CREATE`; nothing on `public` |

`PUBLIC` loses its default database and schema rights. The runtime login cannot create, alter, or truncate objects and
cannot read the migration bookkeeping tables. No role is granted Apache AGE's `ag_catalog`, so graph access stays denied
until a change grants it deliberately. No application login has rights on `dbos`. The DBOS login holds database `CREATE`
because its SDK runs `CREATE SCHEMA IF NOT EXISTS dbos` on every launch, and PostgreSQL checks that right first. It
exists only once its Secret does. Forgejo's database and login are separate
([`forgejo-deployment.md`](forgejo-deployment.md)). The Job runs `roles.sql` with terse errors and server statement
logging off, so a failure never prints a password. It is deleted an hour after it finishes; re-applying the overlay
re-runs it.

Each environment provisions its own material through `navigator ops secrets apply --service postgres`
([`deployment-secrets.md`](deployment-secrets.md)). The PostgreSQL service file always carries the bootstrap password
and server certificate. It adds the four client keys as a set. Each projects to mounted-file Secrets only:

| Secret | Keys | Mounted by |
| --- | --- | --- |
| `navigator-postgres-auth` | `password` | Server, roles Job |
| `navigator-postgres-tls` | `tls.crt`, `tls.key` | Server |
| `navigator-postgres-app` | `password`, `ca.crt` | `web` (opt-in), mirror Job, roles Job |
| `navigator-postgres-migrator` | `password`, `ca.crt` | `navigator-migrate`, roles Job |
| `navigator-postgres-backup` | `password`, `ca.crt` | WAL archive, base backups, roles Job |
| `navigator-postgres-dbos` | `password`, `ca.crt` | `web` (DBOS opt-in), roles Job (optional mount) |

`navigator-postgres-dbos` comes from the optional `NAVIGATOR_POSTGRES_DBOS_PASSWORD`, which needs the client set for its
CA bundle. The roles Job converges the `navigator_dbos` login only when that Secret exists.

None of these values enters the `envFrom` deployment Secret. A pod mounts only its own login, plus the CA bundle.

### `web` opt-in

Set `NAVIGATOR_POSTGRES_ENABLED = "true"` in a deployment's `config.toml` to opt in. `ops ship` then adds
`examples/deploy/k8s/gke/postgres/web/web-postgres.yaml` after `web-env.yaml`. It gives `web` the
`NAVIGATOR_POSTGRES_HOST`, `PORT`, `DATABASE`, `SSLMODE` (`verify-full`), `ROOT_CERT`, and `USER` (`navigator_app`)
values inline, the password from `navigator-postgres-app`, a read-only mount of only that Secret's `ca.crt`, and the
client label. An absent or `false` value renders nothing; any other value refuses the ship. With the variables present,
`web` imports the legacy journal before serving and logs `legacy journal import completed`
([`firm-journal.md`](firm-journal.md)).

### DBOS opt-in

Set `NAVIGATOR_DBOS_ENABLED = "true"` in a deployment's `config.toml` to host the DBOS executor in `web`. `ops ship`
then adds `examples/deploy/k8s/gke/postgres/web/web-dbos.yaml` after `web-env.yaml`. It sets `NAVIGATOR_DBOS_ENABLED`
and the `NAVIGATOR_DBOS_HOST`, `PORT`, `DATABASE`, `SSLMODE` (`verify-full`), `ROOT_CERT`, and `USER` (`navigator_dbos`)
values inline. It takes the password from `navigator-postgres-dbos` and mounts only that Secret's `ca.crt`. Both rows
ship `"true"`; `ops ship` refuses a row whose `navigator-postgres-dbos` Secret is missing. [DBOS
durability](dbos-durability.md) has the operator steps.

### Migration Job

`k8s/postgres/migrate/migrate-job.yaml` runs the release image's `/app/navigator-migrate`, with no arguments, as
`navigator_owner`. It reads the same `NAVIGATOR_POSTGRES_*` variables `web` reads. The image is the placeholder
`navigator-migrate`, pinned to the release at apply time. A failure is not retried. The command lives only in that file.

### Mirror parity and reconcile

With `NAVIGATOR_POSTGRES_ENABLED = "true"` and `NAVIGATOR_POSTGRES_MIRROR = "1"` in the deployment configuration, `ops
ship` requires a clean initial reconciliation, enables bounded synchronous projection retries, and installs recurring
recovery with a monitoring Rule. SurrealDB remains authoritative for mirrored domain rows. Recovery provides eventual
convergence after failures; it does not commit the two databases atomically. The [mirror tooling
guide](postgres-tooling.md#postgresql-mirror-tables-and-reconciliation) describes coverage and recovery limits. A missed
write, or a row written before the mirror was on, leaves drift. The operator command measures and repairs that drift:

```bash
navigator ops postgres mirror parity [--json]              # read only
navigator ops postgres mirror reconcile --dry-run [--json] # read only
navigator ops postgres mirror reconcile [--json]           # re-mirror from SurrealDB
```

It reads the same `NAVIGATOR_SURREAL_*` and `NAVIGATOR_POSTGRES_*` variables `web` reads. Each table reports its
SurrealDB and PostgreSQL row counts and its missing, extra, and differing rows. Divergent rows are named by UUID only. A
Social Handle or `firm_brand` row is keyed by its Brand key, so it is counted and never named. Reconcile runs parents
first: identity, then tenancy, then post. When a stale parent is still referenced, it retries once in the reverse order.
A second run over matching stores writes nothing. The exit status is 0 when both stores match, 1 when drift remains, and
2 when a store could not be read.

`k8s/postgres/mirror/mirror-reconcile-job.yaml` runs `reconcile --json` once, as `navigator_app`. It reads SurrealDB
through the deployment Secret's `NAVIGATOR_SURREAL_*` keys. The image is the placeholder `navigator-surreal-archive`.
That is the release image that carries the `navigator` CLI. Pin it to the release being reconciled, then create the Job:

```bash
kubectl kustomize examples/deploy/k8s/gke/postgres/mirror/<env> \
  | kubectl set image --local -f - reconcile=<registry>/navigator-surreal-archive@<release-digest> -o yaml \
  | kubectl --context <context> -n <namespace> create -f -
kubectl --context <context> -n <namespace> wait --for=condition=complete \
  job/navigator-postgres-mirror-reconcile --timeout=30m
kubectl --context <context> -n <namespace> logs job/navigator-postgres-mirror-reconcile
kubectl --context <context> -n <namespace> delete job navigator-postgres-mirror-reconcile
```

The log is the JSON report. A failed Job is not retried. Read the report, fix the cause (for example a Person or Entity
that PostgreSQL does not hold yet), delete the Job, and create it again.

### Rotation

Changing a Secret does not change a role's password. Rotate one environment at a time:

1. Update the value in that environment's encrypted PostgreSQL service file and run
   `navigator ops secrets apply --deployment <name> --service postgres`.
2. Re-apply the environment's PostgreSQL overlay; the roles Job sets every password from its Secret.
3. Restart the clients of the rotated login (`web` through `ops ship --restart-only`, or the WAL archive pod).

The bootstrap password is changed with `ALTER ROLE postgres` over the local socket, then stored the same way. Rotating
one environment never touches the other: each has its own encrypted file, Secret Manager project, and namespace.

## Backups and point-in-time recovery

Backups go to the deployment's own bucket under `<namespace>/navigator-postgres/`. The bucket name is a deployment
coordinate: `navigator ops gcp setup` creates `<project>-postgres-backups`, and the operator records it as the
`navigator-postgres-backup-target` ConfigMap (`bucket` key). The `navigator-postgres-backup` Kubernetes service account
is the only reader and writer. It has no Workload Identity annotation; setup grants the KSA principal object admin on
that bucket directly.

- **WAL archive.** `navigator-postgres-wal-archive` streams WAL with `pg_receivewal --synchronous` through the physical
  replication slot `navigator_wal_archive` into a retained spool claim. Its uploader copies each finished segment to
  `wal/`, and the in-progress `.partial` segment whenever it changes, every 60 seconds. The server caps WAL held for a
  stalled slot at `max_slot_wal_keep_size=4GB`. Past that cap the archive has a gap; the next base backup closes it.
- **Base backups.** `navigator-postgres-base-backup` runs daily at 09:30 UTC. It takes a compressed tar backup with
  streamed WAL (`pg_basebackup --wal-method=stream`), uploads it to `base/<UTC stamp>/`, and writes `_COMPLETE` last.
  Restores ignore a backup without that marker.
- **Scratch sizes.** The base backup's per-run 50 GiB claim holds the gzip-compressed backup, not the 100 GiB volume,
  and the 10 GiB WAL spool sits above the 4 GB slot cap. Grow the backup claim when a compressed backup nears 50 GiB;
  the restore drill's 100 GiB claim holds the download and the restored data, so grow it then too.
- **Retention** is the bucket's lifecycle rule. Keep at least two base backups and all WAL newer than the oldest. Setup
  deletes objects at 30 days, which keeps every daily base backup younger than that and the WAL after the oldest.

| Objective | Target | Measured by |
| --- | --- | --- |
| Recovery point (RPO) | 2 minutes of committed data | Drill's `recovered_through` against the live last write |
| Recovery time (RTO) | 1 hour to a verified, promoted copy | Job start/completion timestamps |

Iceberg exports complement these physical backups; they do not preserve WAL or recovery semantics.

### Restore drill

The drill restores into scratch storage in the same namespace. It never connects to the live server. Run it in staging
first, and in production before calling PostgreSQL recoverable there:

```bash
kubectl --context <context> -n <namespace> exec navigator-postgres-0 -- psql -U postgres -d navigator -Atc \
  "SELECT now()"                 # note the time; the drill below targets it
kubectl kustomize examples/deploy/k8s/gke/postgres/restore-drill/<env> \
  | kubectl set env --local -f - -c restore RECOVERY_TARGET_TIME='<that time>' -o yaml \
  | kubectl --context <context> -n <namespace> apply -f -
kubectl --context <context> -n <namespace> wait --for=condition=complete \
  job/navigator-postgres-restore-drill --timeout=4h
kubectl --context <context> -n <namespace> logs job/navigator-postgres-restore-drill -c restore
```

The log reports the recovery point, databases, extensions, row counts per table, AGE graph vertex and edge counts, and
`recovery_seconds`. Where the database hosts the DBOS executor, it also reports `dbos.workflow_status` counts by status
and release, and the executor identity and release that own each `PENDING` run: a restored pending run resumes only on
an executor with that identity and release ([DBOS durability](dbos-durability.md#several-replicas)). It verifies
Navigator and, when present, Forgejo separately. The Cloud SDK fetch container runs as its named UID 1000; PostgreSQL
runs as UID 999, with the shared scratch volume group 999. Compare the counts with
`k8s/postgres/restore-drill/verify.sql` run against each live database, and record the result. Omit
`RECOVERY_TARGET_TIME` to recover to the end of the archive; set `RESTORE_BASE_BACKUP` on the `fetch` container to
restore a specific base backup. Delete the Job afterwards; its scratch claim goes with it.

Measure recovery time from the Job's start to successful completion so it includes fetching and verification.
`recovery_seconds` measures extraction, WAL replay, and promotion within the restore container. Compare
`recovered_through` with the latest known committed write; an idle database's transaction timestamp can be older than
its newest archived WAL object.

### PostgreSQL image updates

Use the [image publication procedure](../images/postgres/README.md#update-the-published-image) to obtain a tested digest
and propagate every pin before rollout. Apply the candidate in staging first. Record the PVC UID, server and extension
versions, and SQL and AGE aggregate counts before applying; wait for readiness and compare the same checks afterwards.
Verify recent WAL objects and a completed base backup, then run the scratch restore drill and compare both databases.
Only a verified staging result supports a later production rollout. Preserve the StatefulSet's retained claim; never
delete a PVC or namespace to update the image. PostgreSQL major-version changes need their own upgrade plan.

Physical recovery evidence and archive export evidence are separate. An archive trigger Job completing proves only
trigger submission. Verify runtime source configuration, completed generation manifests, table outcomes, and independent
rehydration before calling PostgreSQL or Forgejo Iceberg exports recoverable.

## Rollout runbook

Staging first. Production starts only after staging passes every step and its restore drill succeeds. Each step names
its environment's context, namespace, and deployment row.

1. **Secrets.** Add the four client keys to the environment's encrypted `postgres-secrets.enc.yaml`: the server CA
   certificate and three new random passwords. Dry-run, then apply: `navigator ops secrets apply --deployment <name>
   --service postgres [--dry-run]`. The self-signed server certificate doubles as the CA bundle, and both clients must
   accept it: issue it from a config whose `x509_extensions` section holds only the four `navigator-postgres` SANs.
   OpenSSL 3's `req -x509` default adds `CA:TRUE`, which rustls (`navigator-migrate`) refuses as `certificate unknown`;
   a `keyUsage` without `keyCertSign` makes libpq (`psql`) refuse it. Copy the same certificate into the row's
   `NAVIGATOR_FORGEJO_POSTGRES_CA_CERT`.
2. **Backup target.** `navigator ops gcp setup` creates `<project>-postgres-backups` with its 30-day lifecycle rule and
   grants `serviceAccount:<project>.svc.id.goog[<namespace>/navigator-postgres-backup]` object admin on it; replacing
   the in-progress `.partial` segment needs delete. It also server-side applies the `navigator-postgres-backup-target`
   ConfigMap, whose `bucket` key names that bucket, into the row namespace for the backup workloads.

3. **Server and backups.** `kubectl kustomize examples/deploy/k8s/gke/postgres/<env>`, review, server-side dry-run,
   then apply. The server restarts once for the replication settings. Wait for `statefulset/navigator-postgres`. A first
   apply creates the 100 GiB claim. Prove pod replacement keeps the same PVC UID, as staging did, before continuing.
4. **Backups verified.** Wait for `navigator-postgres-wal-archive-0` to be ready and `wal/` to fill. Trigger a first
   base backup, then run the restore drill above. Production `web` does not opt in until its own drill passes.

   ```bash
   kubectl --context <context> -n <namespace> create job navigator-postgres-base-backup-initial \
     --from=cronjob/navigator-postgres-base-backup
   ```

5. **Roles and migrate.** The release workflow applies `postgres/access/<env>` after server readiness to converge client
   roles, then pins the migration Job to the same digest as the application image. It waits for migration completion
   before Forgejo or Navigator workloads are updated. To run these steps manually, apply the dedicated role overlay,
   then pin and apply the migration Job as follows:

   ```bash
   kubectl kustomize examples/deploy/k8s/gke/postgres/migrate/<env> \
     | kubectl set image --local -f - migrate=<registry>/<web-image>@<release-digest> -o yaml \
     | kubectl --context <context> -n <namespace> apply -f -
   kubectl --context <context> -n <namespace> wait --for=condition=complete job/navigator-migrate --timeout=15m
   ```

   Delete the completed Job before the next release's migration.
6. **`web` env.** Set `NAVIGATOR_POSTGRES_ENABLED = "true"` in `deployments/<name>/config.toml` through a pull
   request. Then ship that release: `navigator ops ship --deployment <name> --tag <release>`.
7. **Verify.** The `web` log shows the import and triage counts; then smoke the journal through the deployment's API:

   ```bash
   kubectl --context <context> -n <namespace> logs deploy/navigator-web | grep 'legacy journal import completed'
   navigator journal --firm <firm-id> --host <deployment-host> list
   ```

To back out, set `NAVIGATOR_POSTGRES_ENABLED = "false"` and ship. `web` stops connecting to PostgreSQL. Journal entries
already written there stay there, and the journal routes need PostgreSQL ([`firm-journal.md`](firm-journal.md)).

## Multiple service databases on the shared instance

The PostgreSQL pod is the shared server boundary for Navigator and opt-in Forgejo. Navigator uses `navigator`; Forgejo
uses its separately provisioned `forgejo` database and dedicated non-superuser login, which the
`navigator-forgejo-database` Job converges, as described in [`forgejo-deployment.md`](forgejo-deployment.md). The local
KIND tier runs the same arrangement, proven by `navigator dev forgejo smoke`; see [Forgejo on KIND](forgejo-kind.md).
Never grant either application's role access to the other database. Both databases share the same pod, PVC, resource
limits, maintenance window, instance failure domain, and physical backups: a restore recovers both to the same point.
Forgejo's Git, LFS, and attachment files remain on its own retained volume and must be backed up with its database.

The twice-daily Iceberg export must treat each database as an independent source: record its database identity, schema
version, source cutoff, table inventory, and generation manifest; publish separate Navigator and Forgejo namespaces; and
test rehydrating each into a fresh database without crossing service ownership. A single PostgreSQL transaction cannot
provide a common snapshot cutoff across separate databases. Until the per-database exports and restore drills exist,
Forgejo's database is not covered by the existing Surreal-backed Iceberg archive.

## Evidence

Focused CI checks a new pod UID, unchanged PVC UID, and retained SQL and AGE data after replacement. It also scales to
zero, deletes/recreates the StatefulSet, verifies retention, accepts TLS, and rejects unencrypted access. KIND proves
claim lifecycle; GKE CSI provisioning and cross-zone recovery require separate environment drills. Manifest tests in
`cli/tests/staging_manifests.rs` hold the roles, migration, mirror, backup, and drill contracts, and the
staging/production equivalence; `cli/src/devx/ship.rs` tests hold the `web` opt-in.

See [GKE disks](https://docs.cloud.google.com/kubernetes-engine/docs/how-to/persistent-volumes/regional-pd),
[StatefulSet retention contract](https://kubernetes.io/docs/concepts/workloads/controllers/statefulset/), and
[continuous archiving and PITR](https://www.postgresql.org/docs/16/continuous-archiving.html).
