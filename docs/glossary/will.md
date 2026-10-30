---
title: "Will"
description: >-
  A kind:will legal instrument — a last will and testament that renders plain, with no letterhead chrome.
---

A last will and testament, as Navigator's [Notation Kind](notation-kind.md) `will`: a legal instrument the firm drafts,
renders, and routes through lawyer review. [`Kind::Will`](../../rules/src/kind.rs) classifies it under
`Category::Instrument`, beside `trust` and `directive`, and its default [Output Format](output-format.md) is plain, with
no letterhead chrome: the instrument carries no firm frame, so the body must name itself — exactly one `#` document
title, opening the body above the first numbered section, since nothing else supplies one.

The body is a Roman-outline contract under rule `N123`: `##` sections numbered `I.`, `II.`, `III.`, in sequence, with
subsections lettered `A.`, `B.`, … restarting beneath each section (see
[`notation-authoring`](../notation-authoring.md#what-runs-after-intake--the-step-registry)). An executor, a guardian
nomination, a residuary gift — each is a numbered section, not a bold-led paragraph; `N125` flags the drift.

A Will's signing chain runs through the workflow steps: the `witnesses` prefix records respondent-side witness
signatures, the `_signature` / `_signatures` step family records the testator's own signing, and a
[Notarization](notarization.md) may record the notarial acknowledgment where the jurisdiction asks for one. See
[Signature](signature.md) and [`workflows::step::STEP_PREFIXES`](../../workflows/src/step.rs).

The firm's Vesta practice drafts them — see [Vesta Estate Planning](vesta-estate-planning.md). After death, a Will is
where [Estate Administration](estate-administration.md) starts and [Probate](probate.md) usually follows.

- Source: [`rules::kind::Kind`](../../rules/src/kind.rs) · Rule: `N123` in
  [`docs/gate.md`](../gate.md#rule-codes)
