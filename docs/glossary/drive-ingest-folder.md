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
a person made by hand before the matter was opened. Firm-side participants receive access to the working folder. Client
participants receive a separate [Client Submissions](client-submissions.md) folder. The lawyer toolbar links to the
working folder after Navigator verifies its access. Until then, the icon opens the Project's folder settings.

To record an existing folder, enter its Google Drive link or ID in those settings or run `navigator project set
--drive-folder <link-or-id>`. Use `--dry-run` to preview the CLI change. With Drive configured, Navigator checks the
folder against the Shared Drive. Each folder ID belongs to one Project.

- Provider: [`cloud::drive`](../../cloud/src/drive.rs) · provisioning:
  [`store::project_folder_reconcile`](../../store/src/project_folder_reconcile.rs) · import:
  [`store::drive_import`](../../store/src/drive_import.rs)
