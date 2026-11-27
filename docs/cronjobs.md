# Scheduled jobs

The application deployment ships two Kubernetes CronJobs in the `navigator` namespace. Each submits an authenticated,
idempotent invocation to the private DBOS endpoint in the web deployment.

| CronJob | Cadence | Work |
| --- | --- | --- |
| Nightly Project Update | 03:33 UTC daily | Sync open Project calendars, guests, Slack channels and repositories. |
| YouTube comments poll | Every 15 minutes | File new audience comments in the firm inbox. |

The deployment and the lawyer console at `/app/admin/schedules` use the same manifest list in [the scheduled trigger
overlay](../examples/deploy/k8s/exports/kustomization.yaml). The NetworkPolicy admits only the two trigger pods to the
private DBOS port. Each trigger reads its bearer from the deployment's web Secret. Keep the bearer out of the manifest.

DBOS runs the other application schedules from PostgreSQL. See [durable workflows](durable-workflows.md) for the
executor and its recovery behavior. PostgreSQL backups have a separate [operational
schedule](../k8s/postgres/backup/kustomization.yaml).

## Daily Project forge maintenance

DBOS runs `daily-project-forge-parity` at 4:44 a.m. America/New_York. For each open or pitch Project, it verifies both
private repositories and their main revisions. It can fast-forward the mirror from the recorded primary. It records
divergent branches, unverified destinations and missing credentials for investigation.

After verifying parity, the schedule compares the Project manifest with published stable releases. It queues the next
release in order. Each Project, release and starting revision identifies one durable job. See the [maintenance worker
setup](../scripts/project-maintenance/README.md) for the CLI and Vertex repair queues.

## Publishing

The release workflow builds each trigger from `images/Containerfile.trigger` and publishes a versioned image. `hudson
ops ship` pins the trigger images to the selected release. Add trigger manifests, runtime configuration and
operator-facing copy together when introducing a new CronJob.
