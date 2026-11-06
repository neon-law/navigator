---
title: "Re-ask"
description: "Re-ask returns a Notation to collect answers flagged by Lawyer Review before it returns to review."
---

The workflow prefix `reask` (state `reask__client`) is where a [Lawyer Review](lawyer-review.md) that returned
`changes_requested` parks a Notation to re-collect the answers it flagged, before the matter loops back to
`lawyer_review`. Only the flagged answers are re-collected — the client self-serve, or their lawyer — never the whole
questionnaire, and the Notation's pinned template version is unchanged: answers are corrected, the paper is not.
`rejected → END` is reserved for a genuine withdrawal or decline. The flagged set + reviewer note are recorded on the
attributed [Notation Event](notation-event.md) journal (`store::reask`); the CLI drives it with `notation
request-changes` then `notation update`. See [`workflows::step::STEP_PREFIXES`](../../workflows/src/step.rs) and
[`store::reask`](../../store/src/reask.rs).

Re-ask collects only questions on the completed questionnaire route. Corrections are checked together before writing: a
choice may change while preserving the question chain, but a correction requiring a different chain needs a fresh
intake. An answer from another route does not authorize re-collection on that route.

The active walk can display extracted proposals as defaults. Re-ask uses the latest confirmed answers instead: a later
proposal cannot replace a confirmed branch choice or make a question from another route eligible for correction.

A legacy notation whose recognizable seeded body has discarded its questionnaire metadata needs a fresh intake when no
frozen definition remains. Rendering and Re-ask refuse that missing authority rather than borrowing a current graph.
