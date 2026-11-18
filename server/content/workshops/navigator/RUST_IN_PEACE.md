---
kind: workshop
title: Rust in Peace
description: Rust in Peace tours the Rust monorepo behind a litigation-first law practice.
---

# Rust in Peace

A talk by Nick for [Rust NYC](https://www.meetup.com/rust-nyc/events/316056830/)

Rust in Peace is a eulogy to my programming career. Over the past year, teaching coding at Apple, I realized nearly all
my students were vibe coding and had stopped needing me. That was the signal to go back to practicing law.

Our firm runs its entire practice on the [Neon Law Navigator](/navigator), Rust-built software that flexes its great
ecosystem. We built our practice to enable all lawyers to vibe-code custom client experiences and tell our clients'
unique stories. In this presentation, you'll see how this all fits together beginning with how we architected the data,
chose Rust-first vendors, and leverage agentic tooling for coding and lawyering.

Navigator is only possible because the ecosystem is this good, because the LLMs are this good, and because communities
like RustNYC are this good. Please come to my retirement party and let us get crabby together.

## Intro

### May my soul rust in peace

![Ferris the Rust crab in goggles holds a glowing vial, Megadeth Rust in Peace parody](img/rust-in-peace/cover.png)

---

Rust in Peace is a Eulogy to my programming career, as I know I'll never see myself as a full-time developer again.

### Las Vegas Ruby Group

![The Las Vegas Ruby Group's red ruby profile mark](img/lvrug/lvrug.png)

---

Began programming here. Very grateful for Paul, David, Ryan, Alex, Jeremy, Russ, Jason, Rachel, Brian, Dylan and so many
more.

### 5 Years at the Fruit

![A hand holds a kiwi in front of a colorful rainbow sculpture](img/rust-in-peace/kiwi-rainbow.jpg)

---

Twenty days in New York. Thank you to Rob, my friend of twenty years.

### 4 years finance data & platform engineering

![Ten colleagues standing together in an Apple office](img/rust-in-peace/apple-team.jpg)

---

Over 30 petabytes of data, high stress but high performing team.

### 1 year teaching programming

![Colleagues share dinner together at a restaurant](img/rust-in-peace/apple-teaching.jpg)

---

The man in the front is Owen, my beloved boss. Apple is an incredible place.

### What I realized is

![Groovy card: asterisk everyone loves vibing; nearly](img/rust-in-peace/everyone-loves-vibing.png)

---

Nearly everyone loves vibing.

### Decided to be a lawyer in New York

![New York City seen beyond a hillside cemetery](img/rust-in-peace/new-york-lawyer-decision.jpg)

---

I decided to be a lawyer in New York.

### What our firm does

{{firm-product-cards}}

---

Our firm offers Litigation, Fractional GC, Personal Plan, and One-Time Services.

### Neon Law Navigator

{{navigator-product}}

---

Neon Law Navigator is software designed to enable lawyers to be vibe-coding storytellers.

### Agenda

- Why go all-in on Rust
- {Library,Data,DevX,Cloud} tour
- Navigator: Everyone vibes
- Notations: Markdown => PDF with workflows
- Thanks

---

We begin with our experience, we end rusting in peace.

## Why go all-in on rust

### Compiled, correct, and fast

![Ferris the Rust crab racing a hare at tremendous speed](img/rust-in-peace/ferris-races-the-hare.png)

---

If we're vibing, why not just vibe in Rust?

### Amazing Community

What other compiled language has meetups this big? And a discord community of thousands.

![A large Rust community meetup filling a presentation room](img/rust-in-peace/rust-community-meetup.png)

---

Also perhaps other communities like JavaScript and Python are too big and subsequently fragmented.

### macOS development, Linux deployment

- Develop and use the CLI on macOS.
- Deploy services on Linux/amd64.

---

Rust supports many targets. This repository's development and deployment contracts name the targets we maintain.

### Non-profit governance

The Rust Foundation > Private Company

![Ferris shares governance with an equal business community](img/rust-in-peace/ferris-shared-governance.png)

---

Check each dependency's license and service pricing. Foundation governance does not remove licensing obligations or make
hosted services free.

### snake_case > camelCase

`rob_balicki` or `robBalicki`

---

I'll die on this hill.

## The tour: {Library,Data,DevX,Cloud}

### Libraries: Some crates

The public Navigator monorepo uses this `Cargo.toml` stack.

- **HTTP and views** — `axum`, `dioxus`, `tower` / `tower-http`
- **Store and durable execution** — PostgreSQL with AGE, `sqlx`, and `dbos`
- **Telemetry & Backups** — `opentelemetry`, `arrow`, `parquet`
- **Notations** — `typst`
- **Cloud** — `google-cloud-storage`, `reqwest`

---

It's easier thinking in only one language.

### Libraries: Developer parity

Local `KIND` deployment

- **Rauthy** - OIDC
- **Regorus** - OPA
- **DBOS** - Durable workflows
- **PostgreSQL with AGE** - Application records and graph

---

Use the task-owned local environment to verify behavior. Local execution does not prove production IAM or recovery.

### Data: People

A `person` can be one of:

- **Client** - a participant in their Project
- **Clerk** - a supervised worker
- **Lawyer** - member of the bar
- **Admin** - manages the installation; a role is not a law license
- **Owner** - owner of the system

![Ferris joins a five-role council around a worktable](img/rust-in-peace/data-people.png)

---

Role and Project participation are separate checks. An engagement letter and professional licensure are separate facts.

### Data: Project

A project or "matter" is a loose and flexible arrangement of legal services.

Each has at least one client and lawyer DRI who can make decisions.

![Ferris connects a client and lawyer through a flexible project constellation](img/rust-in-peace/data-project.png)

---

Per-project disclosure is important.

### Data: Documents

The abstract deliverable of lawyers.

Contracts, pleadings, forms.

![Ferris transforms legal work into a stack of signed documents](img/rust-in-peace/data-documents.png)

---

Keep the records the client and representation need, with reliable provenance and review.

### DevX: Monorepo

Public Navigator monorepo

![Ferris maintains a living treehouse monorepo](img/rust-in-peace/devx-monorepo.png)

---

A well-tested mono repo keeps things in sync.

### DevX: Glossary & Ontology

www.neonlaw.com/glossary

![Ferris organizes a shared vocabulary in a magical library](img/rust-in-peace/devx-glossary-ontology.png)

---

Keeping terms aligned ensures that we're reviewing work together.

### DevX: Tests

Unit & Integration. High code coverage.

![Eight continuous integration checks passed](img/rust-in-peace/tests-checks-passed.png)

---

If we're vibing, vibe high and as much as possible.

### DevX: Multiple explanations

Compare a long string that can't fit in memory? 6 unique spots.

Here, 6 unique explanations. Tests, code, prs, commits, diagrams, workshops, etc.

![Three Ferris crabs point at one another](img/rust-in-peace/ferris-three-way-pointing.png)

---

If an LLM describes the same concept in multiple different ways, chances are we have mutual understanding.

### DevX: Agent skills

Optimize for both Claude Code & Codex.

Adversarial review between the two is amazing.

![Two Ferris characters review the same code in mirrored workshops](img/rust-in-peace/devx-agent-skills.png)

---

A "tell me what this is prompt" is often illuminating.

### DevX: Works on my machine

Linux, Mac. Local with and w/o Kind. GKE.

Orbstack for local containers

---

Flex Rust's skills to work everywhere.

### DevX: Build for all use cases

Web. CLI. MCP.

![Ferris uses a website, CLI, and MCP tools](img/rust-in-peace/ferris-web-cli-mcp.png)

---

Everyone uses software in different ways.

### DevX: One flow, worktree per PR

One `main` branch. Squash & Merge. Worktree per PR.

Tag only from `main`.

![Ferris tends a main railway with worktree branches](img/rust-in-peace/devx-one-flow-worktree-pr.png)

---

Environment creation should be idempotent. The test should always run all the time.

### Cloud: GCP

Omakase. Nicer customer support.

![Ferris selects dependable cloud infrastructure from a cloud kitchen](img/rust-in-peace/cloud-gcp.png)

---

The `gcloud` CLI and IAM much easier to reason about.

### Cloud: GKE Autopilot

Kubernetes is awesome, even if maintaining isn't.

![Ferris relaxes while a Kubernetes cloudship runs on autopilot](img/rust-in-peace/ferris-gke-autopilot.png)

---

Benefits of using the same manifests and tech like sidecars don't need to learn something new.

### Cloud: SaaS

SaaS will never die, the same way restaurants wont.

SendGrid, DocuSign, Stripe, Slack, Notion, GitHub.

![Ferris assembles specialized services in a storybook marketplace](img/rust-in-peace/cloud-saas.png)

---

We create documents, not rebuild the above.

### Cloud: PostgreSQL, AGE, and DBOS

- PostgreSQL holds application records.
- AGE provides the graph within PostgreSQL.
- DBOS checkpoints durable workflow execution in PostgreSQL.

---

Keep application and DBOS database permissions separate. A workflow checkpoint does not make an external effect
exactly-once: use stable effect keys and the provider's idempotency support, then test recovery.

### Cloud: Iceberg backups

Private application snapshots and separate operational telemetry.

Different GCP account **and** different vendor.

![Ferris sends glowing data containers to a distant iceberg backup vault](img/rust-in-peace/cloud-iceberg-backups.png)

---

Rehearse a restore. Check backup coverage, retention, credentials, and document bytes. A backup is not a guarantee
against data loss, and operational logs must exclude client content.

## Navigator: Everyone vibes

### High-Throughput Low-Cost

Our north star is Access to Justice

![Ferris helps every person be seen and heard](img/rust-in-peace/ferris-access-to-justice.png)

---

And all of our actions should be consistent with working towards access to justice sustainably.

### Lifecycle of a project

![Three recycling arrows labeled Intake, Feedback, and Documents](img/rust-in-peace/project-lifecycle.png)

---

A project could be a quick LLC incorporation. It could be managing 10 cases on behalf of one person. Projects are
flexible on purpose.

### Intake, our customer conversion

Collect info. Web forms, chat prompts, API calls (e.g. ID.me, Docusign)

![Ferris welcomes a visitor through three intake entrances](img/rust-in-peace/intake-customer-conversion.png)

---

Decouple questions from projects to ensure we're not repeating and asking the same things.

### Continous feedback

The best ideas never happen with one person. We work with our teammates and clients.

Custom web apps, client MCPs, phone & video.

![Ferris builds an idea with a lawyer and client around a feedback table](img/rust-in-peace/continuous-feedback.png)

---

The more our clients are engaged with the process, the happier they are with our services.

### Vibing for clients

<https://staging.neonlaw.com>

![Groovy card: asterisk everyone loves vibing; nearly](img/rust-in-peace/everyone-loves-vibing.png)

---

Everyone knows is vibing is cool, let's do more of it.

### Create documents

![Ferris signs a document with a quill and the Rust logo](img/rust-in-peace/ferris-signs-rust-document.png)

---

We'll cover in our `Notations` section. High-level PDFs we sign.

Nearly everything else is ephemeral toward that pursuit.

Apply the governing retention policy and any preservation holds to the full matter record, including relevant source
documents and correspondence. A final PDF does not by itself authorize deleting the supporting record.

## Notations: Markdown => PDF with workflows

### Frontmatter

Illustrative notation structure; inspect the current template before copying a workflow into a matter.

```yaml
title: Retainer Agreement
code: onboarding__letter
respondent_type: person_and_entity
jurisdiction: NV
confidential: true

questionnaire:
  BEGIN: { _: person__client }
  person__client: { _: project__engagement }
  project__engagement: { _: END }
  END: {}

workflow:
  BEGIN: { intake_submitted: intake_persisted__client }
  intake_persisted__client: { retainer_rendered: lawyer_review }
  lawyer_review:
    approved: generate_pdf__retainer_pdf
    changes_requested: reask__client
    rejected: END
  generate_pdf__retainer_pdf:
    pdf_persisted: sent_for_signature__pending
  sent_for_signature__pending:
    signature_received: END
  END: {}
```

---

Imbue markdown with a ton of rules. Incremental progress.

### Markdown

```md
{{custom_datetime__engagement_start_date}}

{{person__client.name}}

Re: Retainer Agreement — {{project__engagement.name}}

Dear {{person__client.name}}:

This Retainer Agreement (the "Agreement") is entered into between the Firm and
{{person__client.name}} (the "Client"), reachable at
{{person__client.email}}, for legal services on the matter referred to as
{{project__engagement.name}}.

...
```

---

Imbue markdown with a ton of rules. Incremental progress.

### A Clippy for law

![Ferris inspects legal pages with a magnifying glass](img/rust-in-peace/clippy-for-law.png)

```bash
navigator project gate --help
Check this repository.

Usage: navigator project gate [OPTIONS]

Options:
      --ci
          Hold the run to what CI can prove.

  -h, --help
          Print help (see a summary with '-h')
```

---

Imbue markdown with a ton of rules. Incremental progress.

### Render the retainer

```text
navigator notation envelope prepare <notation-id>
```

![First page of the rendered Retainer Agreement PDF](img/rust-in-peace/retainer-agreement-preview.png)

---

Prepare the rendered document and signing fields, then review them with the lawyer. Sending requires the lawyer's
explicit approval of the document, recipients, and fields. Preparation alone sends nothing.

## Thanks

### Rust in peace

I still vibe in the background, but the foreground is being your lawyer.

![Ferris rests at Green-Wood Cemetery surrounded by friends](img/rust-in-peace/ferris-green-wood-farewell.png)

---

Imbue markdown with a ton of rules.

Rust in peace, Ferris

Surrounded by friends at Green-Wood Cemetery.

We are doing everything we can to improve access to justice. The more people exercise their legal rights, the more
civically engaged they become. And when they’re civically engaged, they try to do better for society. They want to work
together; they want to be together. We can’t live in a status quo where everyone is just doom-scrolling on their own.
