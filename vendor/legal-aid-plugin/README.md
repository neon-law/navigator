# Vendored: Legal Aid Plugin skills

Static copies of three [Legal Aid Plugin](https://legalaidplugin.org/) skills
from LawDroid's open-source repository
([lawdroidai/legal-aid-plugin](https://github.com/lawdroidai/legal-aid-plugin)).

- **Source commit:** `013d6139d56fc2be174efb75fa9a4b809fb444c5` (shallow-cloned
  2026-10-30)
- **License:** Apache License 2.0 — see `LICENSE` in this directory (copyright
  LawDroid Ltd.). Vendored unchanged; the first-party codebase remains
  MIT OR Apache-2.0.
- **Vendored skills (first wave):** `draft`, `research-start`,
  `eligibility-screening`. The remaining twelve are a later wave.

These are plain markdown instruction files, not a runtime dependency: the
`sandbox-skills` crate embeds them at compile time and renders a selected
skill into a Legal Sandbox session's system context. No LawDroid service is
contacted at runtime. The files reference LawDroid's own local paths (e.g.
`~/.claude/...`) as editorial content; the loader treats the markdown as
prompt material only.

No client data is present — the upstream files are generic legal-aid
instructions with illustrative examples only.
