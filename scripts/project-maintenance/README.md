# Project maintenance worker

The native workflows host owns the 4:44 a.m. New York schedule. It reads the Project inventory and checks the private
GitHub and Forgejo main refs with their provider APIs. A verified lagging mirror advances only if its observed main is
an ancestor of the recorded primary. The DBOS workflow result holds each Project's outcome.

Build the separate consumer with `cargo build --release -p workflows-service --bin project-maintenance-worker`. Run it
against the same DBOS system database as the scheduler, with the usual DBOS settings and a unique executor ID. Install
this directory on a dedicated worker with Python 3.9 or later, Git, GitHub CLI, pnpm and Navigator. Install PyYAML for
manifest validation. Configure Git authentication for the two private HTTPS origins, commit signing, the Navigator site
session and the forge review permissions used by the normal Project push command. Run one consumer per environment and
give it a persistent private work directory that supports file locking across restarts.

## `NAVIGATOR_MAINTENANCE_RUNNER`

Absolute path to the installed executable `runner.py`.

## `NAVIGATOR_MAINTENANCE_WORKDIR`

Private volume for job checkouts, release assets, diagnostics and receipts.

## `NAVIGATOR_MAINTENANCE_SITE`

Navigator site host for verifying the live Project.

## `NAVIGATOR_FORGEJO_ROOT_URL`, `FORGEJO_TOKEN`

Forgejo service root and the worker's scoped API credential.

## `NAVIGATOR_MAINTENANCE_GITHUB_MIRRORS`

For Forgejo-primary Projects, a JSON object mapping Project UUIDs to authorized canonical GitHub HTTPS clone URLs ending
in `.git`. Supply the same bindings to the scheduler and worker.

## `NAVIGATOR_VERTEX_PROJECT`, `NAVIGATOR_VERTEX_LOCATION`, `NAVIGATOR_VERTEX_REPAIR_MODEL`

Vertex project, region and operator-selected small model for repair jobs.

## `NAVIGATOR_MAINTENANCE_REPAIR_SOURCES`

Optional path to operator-owned JSON mapping canonical primary HTTPS URLs without `.git` to technical source paths
approved for model input and edits.

The scheduler uses its configured GitHub provider and the Project's firm-scoped Forgejo integration. Install
`google-auth` and `requests` for Vertex on the worker and grant its workload identity permission to invoke the
configured model.

The release queue runs one update at a time. It checks the published tag, verifies the target CLI, updates pins,
synchronizes skills, runs the live read-only repository gate and uses the signed review and deployment workflow. The
worker checks the deployment revision and both main refs before recording delivery. A later daily run can advance the
Project to the next published stable tag.

A failed update check can enqueue one repair for the same Project, release and base. Repair input comprises gate codes,
normalized pin candidates, public release notes and explicitly enrolled technical files. The worker validates model
edits locally and runs the same gate and review workflow. Source binding and configuration failures stop the job.
Inspect the durable failure and private diagnostic, correct the cause, and resume the job or retry it against current
refs. Each worker's filesystem, credentials and repair policy must be scoped to the Projects it operates on.
