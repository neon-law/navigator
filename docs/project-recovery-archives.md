# Project recovery archives

A [Project Recovery Archive](glossary/project-recovery-archive.md) is the internal, verified copy of one Project's
complete recoverable record. This page covers its storage lane, its guarantees, and how it differs from the other copies
Navigator keeps.

## What each copy is for

- **Project Recovery Archive** holds every document revision, Notation source and output, Project-scoped restore rows,
  and a verified Git bundle reference. Audience: internal recovery only.
- **[Handoff Bundle](glossary/handoff-bundle.md)** holds the current legal files a recipient's lens admits, with an
  immutable checksum manifest. Audience: the recipient.
- **[Closed Repository](glossary/closed-repository.md)** holds a working-tree ZIP of the repository's final commit,
  filed as a document. Audience: the matter record.
- **Disaster recovery** holds whole database and bucket backups. Audience: operators restoring a deployment.

A recovery archive never holds credentials, another Project's or Firm's records, or the DBOS system schema.

## Storage lane

The archived-projects lane is a private bucket per deployment. `archived-projects` names its purpose, not a shared
bucket.

| Backend | Setting |
| --- | --- |
| `gcs` | `NAVIGATOR_ARCHIVED_PROJECTS_BUCKET` |
| `s3` | `NAVIGATOR_ARCHIVED_PROJECTS_BUCKET`, optional `NAVIGATOR_ARCHIVED_PROJECTS_ACCESS_KEY`, `_SECRET_KEY` |
| `fs` | `NAVIGATOR_ARCHIVED_PROJECTS_FS_ROOT` |

`cloud::archived_projects_from_env` fails closed. An unset bucket or root is an error. So is a bucket that another lane
already names (documents, exports, assets, applications, LFS, or either analytical archive), or an `fs` root equal to
`NAVIGATOR_STORAGE_FS_ROOT`. Nothing falls back to another lane.

`navigator ops gcp setup` creates the bucket with public access prevention enforced and no lifecycle delete rule.
Retention is decided by the governed purge, never by bucket age.

## Generations

Archive state lives in PostgreSQL, in `project_archive_generation` and `project_archive_item`.

1. **Request.** `store::project_archives::request` records a durable request under a caller-chosen operation key. The
   same key returns the same generation. A Project with a repository must supply the private receipt `navigator ops
   forgejo bundle archive` wrote, so its source and history are part of the archive.
2. **Plan.** Each run reads the Project's current record and lists every expected item. The inventory digest and the
   cutoff are recorded. The cutoff changes only when the inventory does.
3. **Copy.** Bytes go to `firms/<firm>/projects/<project>/objects/<sha256>`. Each item is read back and checked by
   size and SHA-256 before its checkpoint is written. Identical bytes share one object. The Git bundle stays in its own
   lane and is read back at the receipt's exact generation.
4. **Commit.** A run re-reads the Project. If anything changed, it re-plans; otherwise it writes the manifest to a
   content-addressed key under `generations/<generation>/`, reads it back, and commits in one conditional update.
   Database triggers refuse any later change to a committed generation or its items.

Every step is an idempotent copy-and-verify. A retry after a crash, a provider error that hides a successful write, or a
lost checkpoint repeats only that work. Concurrent runners converge on one manifest. A missing or corrupt source stops
the run and records the error; the generation stays uncommitted.

`store::project_archives::verify` re-reads every byte of a committed generation. `probe` round-trips a small object to
check the lane.

## Requests and the schedule

- `POST /app/api/projects/{id}/recovery-archives` — records a durable request (`operation_key`, optional `source_backup`
  receipt) and returns `202`.
- `GET /app/api/projects/{id}/recovery-archives` — lists the Project's generations, newest first.
- `POST /app/api/projects/{id}/recovery-archives/{generation}/verify` — re-reads every byte of a committed generation.

A request returns before any byte moves. The `ProjectRecovery` DBOS schedule in `web` resumes every unfinished
generation each quarter hour, one step per generation, so a pod loss or provider timeout leaves a request the next slot
picks up. It runs when `NAVIGATOR_DBOS_ENABLED=true` and `NAVIGATOR_DBOS_PROJECT_RECOVERY_ENABLED=true`; an enabled
schedule fails the boot without PostgreSQL or the archived-projects lane. Without the schedule, requests stay pending.
See [DBOS durability](dbos-durability.md#projectrecovery).

## Restore

`store::project_archives::restore` rebuilds the Project from one committed generation into any target. It verifies the
manifest and every object before writing. Bytes return to their original storage keys; rows return under their original
ids, with the same revision chains, checksums, provenance, visibility, Soft Deletion stamps, and Legal Holds. A purged
revision comes back as its tombstone only. The target must already hold the Project's Firm and client Entity. An
existing row or object that differs stops the restore rather than being replaced, and a rerun converges.

Restore the repository from the reported Git bundle receipt with `navigator ops forgejo bundle restore`.

## Soft-deleted chains and the purge

A soft-deleted chain is archived to the same lane and the same content-addressed objects, then its verified active
copies are removed (see [Soft Deletion](glossary/soft-deletion.md)). When the governed purge removes archived bytes,
`archived_object_purge` records it: `verify` lists those items under `purged`, and `restore` brings each such revision
back as the tombstone the purge left, never with bytes.

## Handoffs

`POST /app/api/projects/{id}/handoffs` (lawyer tier) prepares a [Handoff Bundle](glossary/handoff-bundle.md) for one
participant. The lawyer or that recipient reads `GET …/handoffs/{handoff}` and downloads `GET
…/handoffs/{handoff}/bundle`. The bundle never includes a recovery archive, source, or history.

## Closeout

Closing a Project, preserving its archive, handing files to a recipient, and deleting its repository are separate
actions. The recipient of a handoff acknowledges the exact manifest digest they received with `POST
…/handoffs/{handoff}/acknowledge`; a retry converges, and a different digest is refused.
`store::project_closeout::readiness` names every blocker in front of the irreversible steps:

- **Archive the Project** waits until it is closed, a committed generation was cut after the close with no document
  changed since, and every required handoff is acknowledged.
- **Delete its repository** waits for all of the above, plus until the generation's verified Git bundle holds the live
  HEAD and no Legal Hold stands.

Requesting a generation or a handoff is never blocked. A repository-less Project needs no source backup. Without
PostgreSQL the records cannot be read, so the steps are refused rather than assumed safe.

## Authority

Requesting, reading, verifying, and restoring need `FirmCapability::ManageRecoveryArchives` on the Project's Firm: Owner
system-wide, or an Admin membership on that Firm. A generation of another Firm reads as not found.
