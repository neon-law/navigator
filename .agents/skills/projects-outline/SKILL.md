---
name: projects-outline
description: Explain or plan the Project repository source contract and its Matter Pages.
---

# Project source outline

Read [`docs/project-repositories.md`](../../../docs/project-repositories.md),
[`docs/glossary/matter-page.md`](../../../docs/glossary/matter-page.md), and
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md).

- A Project repository contains source only: templates, `navigator.yaml` (its `pages:` declare the Matter Pages), and
  checked-in configuration.
- It writes no application code; a page shape Navigator cannot declare yet is a new Section or gate rule in Navigator.
- Navigator owns data, authorization, and writes. Use its read and command boundaries; do not add a matter-data backend.
- Build and test fast with synthetic or firm-owned fixtures. Never put legal files, client data, real contact details,
  or production identifiers in repository source or planning notes.
