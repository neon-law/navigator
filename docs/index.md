# Documentation

The website publishes one of these, the glossary, at `/glossary`; the rest are contributor docs read in the repository.
This page is the map: every document under `docs/`, grouped by topic, with a stable place for anything new to land.
`cli/tests/docs_index_completeness.rs` fails the build when a file exists under `docs/` with no entry here, so this list
cannot decay the way the old seven-line stub did.

## Start here

- [`glossary/`](glossary/README.md) — canonical vocabulary, one file per term, published on one page at `/glossary`.
- [`deadline-proposals.md`](deadline-proposals.md) — cited date proposals and lawyer confirmation.
- [`agent-workflows.md`](agent-workflows.md) — the five action recipes every agent task reduces to.
- [`gitops.md`](gitops.md) — branch, PR, release, and deploy flow.
- [`workspace-layout.md`](workspace-layout.md) — what each crate in the monorepo owns.

## Engineering practice

- [`rust-programming.md`](rust-programming.md) — the workspace's Rust conventions and error-handling rules.
- [`observability.md`](observability.md) — how Navigator emits and reads telemetry.
- [`test-database.md`](test-database.md) — the one-engine-per-test SurrealDB contract.
- [`licensing.md`](licensing.md) — the MIT OR Apache-2.0 grant and what it means operationally.
- [`licensing/redistribution-audit.md`](licensing/redistribution-audit.md) — redistribution evidence and
  rights-review gaps.

## Local development and environments

- [`environments.md`](environments.md) — the deployment environments and what distinguishes them.
- [`env-driven-devx.md`](env-driven-devx.md) — one config surface serving local dev, CI, and production.
- [`cloud-operations.md`](cloud-operations.md) — day-to-day cloud operations against a live deployment.
- [`multi-cloud.md`](multi-cloud.md) — running Navigator on AWS, Azure, or self-hosted Kubernetes.
- [`gke-prod.md`](gke-prod.md) — the GKE production deployment shape.
- [`postgres-kubernetes.md`](postgres-kubernetes.md) — the pinned PostgreSQL image and retained Kubernetes storage.
- [PostgreSQL ownership](postgres-tenancy.md) — per-table scope and ownership checks.
- [PostgreSQL tooling](postgres-tooling.md) — SQLx access and explicit timestamped migration execution.
- [PostgreSQL compatibility](postgres-compatibility.md) — application, extension and durable-state recovery tests.
- [Project recovery archives](project-recovery-archives.md) — the archived-projects lane, verified generations,
  and restore.
- [DBOS durability](dbos-durability.md) — DBOS system schema, the effect-key rule, and the Restate port order.
- [DBOS cutover matrix](dbos-cutover-matrix.md) — which Restate services, CronJobs, and callers have switched to DBOS.
- [`forgejo-deployment.md`](forgejo-deployment.md) — the Forgejo deployment foundation and account-admission gates.
- [`forgejo-kind.md`](forgejo-kind.md) — Forgejo, its Actions runner, and its smoke in the local KIND tier and CI.
- [`manual-roll.md`](manual-roll.md) — rolling, verifying, and rolling back one deployment by hand.
- [`dns.md`](dns.md) — pointing an instance at a real domain.
- [`deployment-secrets.md`](deployment-secrets.md) — the `deployments/` tree and secret material.
- [`provider-environment-parity.md`](provider-environment-parity.md) — keeping third-party providers consistent
  across deployments.
- [`third-party-integrations.md`](third-party-integrations.md) — one provider attachment per deployment.
- [`social-account-onboarding.md`](social-account-onboarding.md) — connecting and verifying each Brand's social
  accounts.
- [`tiktok-publishing.md`](tiktok-publishing.md) — the manual TikTok handoff and why Direct Post stays disabled.
- [`deploy/gke-ship-example.md`](deploy/gke-ship-example.md) — a worked GKE ship, the roll-only model.
- [Self-hosting](self-hosting.md) — fresh clone to a running local `web` without private credentials, then images,
  migrations, tenancy, backups, durable workflows, and upgrades.
- [`oss-install.md`](oss-install.md) — installing Navigator on your own cloud.
- [`release-automation.md`](release-automation.md) — the scheduled release cut and the staging-then-production
  rollout: gate, identities, settings, rollback, and how to turn each on.
- [`public-snapshot.md`](public-snapshot.md) — building, verifying, and publishing the fresh public root commit.

## Authorization and policy

- [`access-model.md`](access-model.md) — the role and participation model: who can see what.
- [`rego-policy.md`](rego-policy.md) — the embedded Rego policy that decides authorization.
- [`oidc.md`](oidc.md) — OIDC sign-in and database-role authorization.
- [`command-boundary.md`](command-boundary.md) — the REST/OpenAPI command boundary, and exporting the OpenAPI
  document for a consumer to pin.
- [`public-contributor-safety.md`](public-contributor-safety.md) — safe public experimentation without touching
  client data.
- [`public-snapshot-audit.md`](public-snapshot-audit.md) — rule `P001`: what a tracked file may not carry into the
  public baseline, and how to resolve a finding.

## Authoring notations and Markdown

- [`notation.md`](notation.md) — the notation vocabulary: templates, questionnaires, workflows.
- [`clauses.md`](clauses.md) — versioned reusable legal wording and explicit questionnaire inputs.
- [`notation-authoring.md`](notation-authoring.md) — authoring a notation template's body and structure.
- [`business-notations.md`](business-notations.md) — the nine business Notations and their drafting choices.
- [`frontmatter.md`](frontmatter.md) — the frontmatter cover sheet on every file, by document kind.
- [`gate.md`](gate.md) — the canonical reference for `navigator project gate` and `navigator validate`: the shared
  passes, flags, and every rule code.
- [`marketing-copy.md`](marketing-copy.md) — the English marketing catalog and how a copy-only edit is gated.
- [`pleading-caption-latex-proposal.md`](pleading-caption-latex-proposal.md) — structured court captions shared by the
  PDF and Word projections of a Template.
- [`editing-workflows.md`](editing-workflows.md) — editing a legal workflow's state machine.
- [`durable-workflows.md`](durable-workflows.md) — Restate-backed durable execution for legal workflows.
- [`finance-aggregate.md`](finance-aggregate.md) — the FinanceAggregate workflow, its finance read, and how to run it.
- [`agent-decision-councils.md`](agent-decision-councils.md) — the review-council patterns and when to use them.
- [`gov-forms.md`](gov-forms.md) — government forms: vendor, map, fill, file.
- [`pleading-caption-latex-proposal.md`](pleading-caption-latex-proposal.md) — proposed shared court captions
  for the PDF and Word projections of a pleading Template.

## Project repositories and Matter Pages

- [`project-repositories.md`](project-repositories.md) — the Project workspace and repository contract.
- [`project-skills.md`](project-skills.md) — the Project Skill catalog: layout, frontmatter, jurisdiction codes, and
  pinning a playbook onto a Project.
- [`design-mockups.md`](design-mockups.md) — the one UX lane: an approved design translated into Dioxus.
- [`design.md`](design.md) — the design system.
- [`assets.md`](assets.md) — public asset references and the approved workflow.
- [`aster-character.md`](aster-character.md) — Aster’s canonical artwork, poses and consistency checks.
- [`signed-url-delivery-audit.md`](signed-url-delivery-audit.md) — the signed-URL document delivery audit: TTLs,
  authorization, `project sync` downloads, portal CSP, and publisher identity.
- [`bulk-contact-import.md`](bulk-contact-import.md) — turning a raw contact list into seeded Persons.
- [`retainer_intake.md`](retainer_intake.md) — the retainer intake walkthrough.

## Navigator MCP, A2A, and MCP clients

- [`mcp-a2a-interaction.md`](mcp-a2a-interaction.md) — Navigator MCP over A2A: confirmations and errors.
- [`claude-mcp-client.md`](claude-mcp-client.md) — Claude as a Navigator MCP client over MCP.
- [`mail-cli.md`](mail-cli.md) — the draft-only Gmail CLI, its token contract, and its limits.

## Billing, trust, and e-signature

- [`firm-journal.md`](firm-journal.md) — the Firm-owned journal (PostgreSQL, or SurrealDB without it), selected
  evidence, and reconciliation.
- [`trust-accounting.md`](trust-accounting.md) — the client trust (IOLTA) mirror: one pooled account per state,
  each matter's held balance, and one withdrawal split across the invoices it settles.
- [`xero-billing.md`](xero-billing.md) — Xero billing setup, invoice flow, and production cutover.
- [`docusign-esignature.md`](docusign-esignature.md) — DocuSign e-signature setup and signing flow.

## Observability pipelines and data

- [`cronjobs.md`](cronjobs.md) — scheduled jobs (CronJobs) and what runs on them.
- [`surreal-archives.md`](surreal-archives.md) — SurrealDB operational archives.
- [`iceberg-archive.md`](iceberg-archive.md) — the Iceberg archive design.
- [`email-summary.md`](email-summary.md) — the support-email summary lane and its end-to-end proof on KIND.
- [`email-events-pipeline.md`](email-events-pipeline.md) — the email delivery event stream into BigQuery.

## Worked demos

- [`naturalization-intake-demo.md`](naturalization-intake-demo.md) — walking the naturalization intake locally.
- [`live-inquiry-coverage.md`](live-inquiry-coverage.md) — proposed live transcript coverage during a matter session.
- [`solana-attestation.md`](solana-attestation.md) — on-chain attestation on Solana.

## Editor integration

- [`lsp/README.md`](lsp/README.md) — `navigator-lsp`, the Language Server Protocol backend for Navigator's Markdown and
  Notation diagnostics.
