---
title: "Firm Library"
description: "A Firm Library is the repository of Drafts and defaults every matter of one Firm starts from."
---

The Drafts and defaults every matter of one [Firm](firm.md) starts from, kept in the repository `library` in that Firm's
organization: `<firm-slug>/library`, beside the Firm's [Project](project.md) repositories. `library` is therefore a
reserved Project code (`cloud::workspace::RESERVED_PROJECT_CODES`). A Firm changes its own templates and its default
retainer by committing to this repository, without a change to Navigator.

The repository holds:

- `drafts/*.md` — [Drafts](draft.md) in exactly the Project repository format, validated by the same rules.
- `library.yaml` — a closed manifest. `retainer: <code>` names the [Template](template.md) every new matter of the Firm
  opens on: a Draft in the library, or a shared-catalog code. Its `kind` must open a matter. An unknown key is refused.

`store::firm_library::sync` reads the library at `main`'s HEAD from the repository store, the same read a Project
repository's Drafts get, and persists each Draft as a Firm-scoped `template` row (`template.firm_id`) pinned to its
commit SHA and body digest. Byte-identical Drafts write nothing. One invalid Draft, an invalid manifest, or a retainer
that names nothing refuses the whole sync, and the door that asked keeps the shared default.

A Template code resolves Project first, then the Firm that owns the Project (`project.firm_id`), then the shared catalog
(`store::templates::resolve`). A Firm's rows are never read for another Firm's Project and never listed as the shared
catalog. The lawyer retainer form (`/app/lawyer/retainers/new`) offers the Firm's onboarding Drafts and selects its
`retainer`; the client start door opens on that `retainer` instead of the service's mapped template. A Firm with no
library repository, or one with no commits, behaves exactly as before.

**Not a [Project Skill](project-skill.md).** A Project Skill is a jurisdiction and practice-area playbook one matter
chooses and pins in its own `navigator.yaml`. The Firm Library is not chosen per matter: it is what every matter of that
Firm starts from, and a Project's own `drafts/` still overrides it code by code.

- Code: [`store::firm_library`](../../store/src/firm_library.rs) · [`store::templates`](../../store/src/templates.rs)
