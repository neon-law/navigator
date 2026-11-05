---
name: human-readable
description: >-
  Write concise, readable documents using plain prose, tables for comparisons, and diagrams for relationships.
---

# Human-readable documents

Follow **Find the term first** in `AGENTS.md`. Write for the person who must act on the document.

- Put the answer or rule first, then the reason and next action.
- Use direct verbs, familiar words, one idea per sentence, and descriptive headings.
- Say each thing once. Remove throat-clearing, hedges, rhetorical reframing, and empty sections.
- Describe the current system. Keep decision history in Git and the issue tracker; state active migration steps as
  instructions.
- Explain unfamiliar terms where they appear. Name exact paths, counts, dates, and scope when they affect the decision.
- Attribute claims to sources. State each source's coverage and limits.
- Prefer positive statements. Use contrast to correct a specific factual error.

## Choose the medium

| Content | Format |
| --- | --- |
| Process | Short numbered list |
| Configurations or comparisons | Table |
| States, dependencies, or branching flows | Diagram |
| Other content | Prose or short bullets |

Render diagrams to PNG or SVG for viewers without Mermaid support. Label nodes and edges; explain meaningful colors and
shapes. Split diagrams that cannot be read at a glance. Reserve blockquotes for actual quotations.

For correspondence, follow [legal-writing](../legal-writing/SKILL.md#correspondence-is-terse): state the fact or ask,
cite the rule, and close with one ask and deadline.
