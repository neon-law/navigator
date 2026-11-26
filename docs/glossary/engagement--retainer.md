---
title: "Engagement / Retainer"
description: "Engagement / Retainer is the client-English synonym for a Notation bound to a Project."
---

Client-English synonym for **[Notation](../notation.md#notation) bound to a Project**. An Engagement is what the firm
sells; under the hood, running an Engagement means creating a Notation, walking its Questionnaire, advancing its
Workflow, and rendering its document.

The **engagement is a matter's first Notation**: a [Project](project.md) is opened first (its own step — `navigator
project create` — which seeds the client and lawyer participation), then the engagement is created on it like any other
Notation (`navigator site notation create <retainer_code> --project <code>`). Accepting an ARAG or LegalEASE referral
opens the Project and its [Legal Plan Onboarding](legal-plan-onboarding.md) Notation. An open Project with no Notations
is a pitch and may start with an offboarding Notation so it can be closed without an engagement.

Projects whose stored code begins with `navigator` may start directly with a working Notation. The prefix exempts them
from onboarding. Authorization and the evidence needed to mark an engagement signed still apply.

For other Projects, the engagement-first rule is enforced at create time on the **self-serve doors** — `web`'s
project-scoped create route and the CLI, which share `workflows::notation_session::create_notation_from_repo`. A first
Notation must be a template whose declared `kind` opens a matter — see
[Onboarding](../../templates/notations/neon_law/onboarding__letter.md) — except that an open Project with no Notations
may start with an offboarding letter. That exception supplies the document a pitch needs before it can close. One
classifier answers which kinds open matters, `rules::kind::Kind::opens_a_matter`; see
[`docs/frontmatter`](../frontmatter.md) for the `kind` vocabulary. Later Notations — filings, letters — may be any kind.

**[Navigator MCP](navigator-mcp.md) is not bound by that rule**, because it is lawyer-directed rather than self-serve.
`create_notation` opens the notation through the policy-free `start_notation` primitive, so an attorney driving the
agent may bind a filing or letter as a matter's first Notation; gating the agent door would forbid the agent's ordinary
use. What constrains Navigator MCP is authorization, not kind: the actor must be lawyer and in scope for the Project
(`store::projects::can_access_as_lawyer_in_surreal`), and the respondent is always the Project's client-side DRI.

A **Retainer** is an Engagement whose bound Template is an onboarding letter. A [Scope Amendment](scope-amendment.md)
records additional work after the Retainer is signed. The [retainer intake workflow](../retainer_intake.md) leads to
lawyer review and signature.

The schema noun in both cases is `Notation`. Client-facing copy speaks Engagement and Retainer because clients do; the
database and the workflow runtime speak Notation.
