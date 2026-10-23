---
title: "Portal"
description: >-
  A Portal was a per-Project client-facing application, now deprecated in favour of Matter Pages.
---

**Deprecated.** A Portal was the one front-end application a [Project](project.md) once built from its own
[Repository](repository.md) and Navigator served beside the matter page. Navigator no longer builds, publishes, or
serves one. Use [Matter Page](matter-page.md) instead: a Project declares each client-facing screen as a `pages:` entry
in its `navigator.yaml`, and Navigator renders it from [Sections](section.md) through the reader's own [Lens](lens.md).

Two neighbours keep their names:

- The **matter page**, `/app/projects/{code}`, is Navigator's own show page for a Project, and the first tab before any
  declared Matter Page.
- The **`portal` crate** is the Rust web application that serves `/app/` as a whole. Its name is historical; in prose,
  call it the web app.

- Replacement: [Matter Page](matter-page.md) · Lane for new screens: [`design-mockups`](../design-mockups.md)
