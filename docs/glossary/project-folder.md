---
title: "Project Folder"
description: "A retained binding between a Project's typed folder slot and its Google Drive folder."
---

A Project Folder binds one [Project](project.md) slot to one Google Drive folder and its parent. The working root, each
[Matter Folder](matter-folder.md) in the fixed tree, and Client Submissions have separate slots. Matter Folder names
retain their canonical paths; Client Submissions is separate from the firm working tree.

PostgreSQL reserves a generated provider ID before remote create. A retry returns the same binding; a different provider
identity or parent requires explicit reconciliation rather than overwriting the old binding. One Drive folder cannot
belong to two Project slots. A parent in the working tree belongs to the same Project.

A reserved binding records identity, not remote existence or proof of isolation. Provider reconciliation checks the
folder identity, parent and access before setting `verified_at`. Resources and imports read only verified bindings. A
retry clears prior verification before checking again, so a failed check leaves the binding internal. Navigator's
participation checks continue to govern its own reads and imports.
