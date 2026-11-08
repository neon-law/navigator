---
title: Kit
description: "A Kit is the versioned Project Skill a matter applied, recorded on the matter row with the version string."
---

A **Kit** is the versioned [Project Skill](project-skill.md) a [Project](project.md) applies: one public playbook, one
version. The catalog ships the playbook text and its bundled [Notation](notation.md#notation) codes; nothing in a Kit
names a [Firm](firm.md) or its economics. Financial premises — target minutes, staffing ratios — stay with the Firm as
[Firm Assumptions](firm-assumption.md), which the Kit references by dotted key and resolves at run time. A missing value
renders blank. It never renders another Firm's number.

`navigator project skill use` pins the Kit into `navigator.yaml` and, when the Project names a live deployment, records
`{jurisdiction}/{practice_area}/{version}` on the matter row as `kit_version`. The row is the durable answer to "which
playbook produced this matter's work": the pin is local configuration, the row is the report.

- Vocabulary: [`store::projects::is_valid_kit_version`](../../store/src/projects.rs)

- Reference: [`docs/project-skills.md`](../project-skills.md)
