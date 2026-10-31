---
title: "Drive Ingest Folder"
description: >-
  The Drive Ingest Folder is the folder in the Firm's Projects shared drive named for the Project code, where Workspace
  users drop files for Navigator to import.
---

One folder per [Project](project.md) in the Firm's Projects shared drive, named for the Project code so no client name
reaches the drive listing. Its Drive file id is `project.drive_folder_id`, and the repository records the same id as
`navigator.yaml` `project.drive_folder_id`.

Drive is import-only: membership lets people drop files in, and Navigator pulls from the folder — it never treats the
folder as a live store of client assets.

`navigator project setup` creates the folder when none is recorded, or adopts an unrecorded one named for the code that
a person made by hand before the matter was opened, recording the id before sharing anything and sharing it as a writer
with every project participant who has an email. A reconcile reports the difference — `created` for a folder it made,
`adopted` for one it only found and recorded, `present` for one already recorded. `navigator project set --drive-folder
<id>` records an id by hand (`--dry-run` previews the write), and a setup or set that names a folder another matter
already records is refused: the folder link, not its display name, is what scopes an import.

- Provider: [`cloud::drive`](../../cloud/src/drive.rs) · provisioning:
  [`store::project_surfaces`](../../store/src/project_surfaces.rs) · import:
  [`store::drive_import`](../../store/src/drive_import.rs)
