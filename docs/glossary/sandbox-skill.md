---
title: "Sandbox Skill"
description: "A vendored instruction file a Legal Sandbox session renders into its system context."
---

A Sandbox Skill is a static markdown instruction file vendored into the Navigator repository and rendered into a [Legal
Sandbox](legal-sandbox.md) session's system context. The first skills come from LawDroid's open-source [Legal Aid
Plugin](https://legalaidplugin.org/) (draft, research-start, eligibility-screening); they live under
`vendor/legal-aid-plugin/` with their Apache-2.0 license and source commit recorded in that directory's README.

A Sandbox Skill is prompt material only: the `sandbox-skills` crate embeds the files at compile time and the model
gateway renders the selected skills per session. There is no runtime dependency on LawDroid services, and a missing
skill id degrades to no skill, never an error.

A Sandbox Skill is distinct from a [Project Skill](project-skill.md), which is a jurisdiction or practice-area playbook
a Project pins onto its own `navigator.yaml`.
