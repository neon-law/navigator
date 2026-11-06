---
title: "Migration Verb"
description: "The Migration Verb is the navigator project migrate-letter command's canonical onboarding move."
---

The `navigator project migrate-letter` command: it finds the one [Legacy Letter](legacy-letter.md) in a Project's Draft
directory, prioritizing the engagement Matter Page's declared Draft. It retypes the frontmatter `kind` to `onboarding`,
moves the file to the canonical `onboarding__letter.md` path, and leaves the `derived_from` lineage and body untouched.
It refuses to guess — with no legacy letter, or more than one, it errors and names the documents instead. An unedited
stub already at the canonical path is replaced; an authored canonical draft is never overwritten. Every Matter Page
reference to the moved Draft is updated, preserving unrelated manifest text. Missing, ambiguous, malformed or unsafe
declarations refuse before file changes. Staged replacements restore the destination and manifest if removing the source
fails. `Kind::opens_a_matter` and the rule engine are untouched, and resolution reads the canonical `kind` after the
move.
