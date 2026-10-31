---
title: "Drive ingest"
description: >-
  Drive ingest sweeps a Project's Google Drive dropbox folder into the matter as screened, receipt-verified documents.
---

Drive ingest is the channel that sweeps a [Project](project.md)'s recorded Google Drive dropbox folder into the matter
through the drive-import door (`POST /app/projects/{code}/documents/drive-import`, engine
[`store::drive_import`](../../store/src/drive_import.rs), CLI `navigator project drive ingest`). Every direct child of
the folder is downloaded, verified against Drive's size and MD5 receipt, screened for malware, and filed as an
[Ingestion](ingestion.md) named by its **original Drive filename** — never a `drive-{file.id}` slug — into the matter
folder its document kind maps to. The CLI forces `visibility: internal`; client-visible documents are shared
deliberately, not swept in with the folder. Files that would collide stay out together: every member of a
case-insensitive same-name group is refused (`name_collision`) rather than silently merged, and a filename that could
not be a document in a Project checkout is refused before its bytes move (`unusable_name`). With `--dry-run` the pass
downloads, verifies, and screens but writes nothing, so its report is exactly what a real pass would file, leave
unchanged, and refuse.
