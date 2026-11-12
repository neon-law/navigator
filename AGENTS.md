# Neon Law Navigator

Navigator is a general-purpose agentic lawyering engine. This public Rust repository contains reusable code, notation
templates, tests, website copy, documentation, and Agent Skills. Keep client data and legal work product in
Navigator-managed systems and approved stores. Follow [public contributor safety](docs/public-contributor-safety.md).

## Repository boundary

- Private marketing strategy and business plans belong in Notion; use
  [notion-marketing-plans](.agents/skills/notion-marketing-plans/SKILL.md).
- Engineering issues and implementation plans belong in Linear. Approved public copy belongs in locale catalogs.
- Matter notes and transcripts belong on the Project's own private or shared Notion page. See
  [what Notion holds](docs/third-party-integrations.md#what-notion-holds).
- Keep private plans, firm records, operational details, and client content out of Git, PRs, comments, and captures.

Use Cargo, isolated worktrees, focused TDD, and shared code. Follow the [Agent Skills](.agents/skills) and name domain
tables from the glossary. The load-bearing members of the 32-crate workspace: `store` owns PostgreSQL-backed accessors
and the graph; `model` holds the domain types, validators, and parsers that open no connection, which `store` and
`cloud` re-export and the lean `navigator` links instead of them; `portal` is the mountable Axum application
(authenticated API, inbound email, Dioxus server-side rendering); `webapp` is the Dioxus client bundle; `neon` is the
brand binary that publishes the public face; `server` runs the integration tests against the mounted app; `cli` is the
lean `navigator` binary (Projects, Jujutsu repositories, `validate`) and the library that holds their rules; `deploy` is
`navigator-deploy`, the operator CLI (`dev`, `ops`, site, mail, notation); `scorpio` is the Ephemeris CLI; `views`,
`workflows`, `workflows-service`, `billing`, `cloud`, and `archives` round out the surfaces the docs name. `Cargo.toml`
is the authoritative member list.

Target macOS for development and the CLI, and Linux/amd64 for deployed services. Add no Windows paths, jobs, or release
archives.

## Current contracts

Describe the system in the present tense. Git holds decision history and superseded behavior. State the reason for a
current constraint once, at its source.

Test observable behavior and meaningful boundaries. Remove tests that only preserve retired copy, mirror constants or
fixtures, or duplicate another check. Keep coverage for authorization, isolation, durability, and active migrations.

## Find the term first

Before naming a table, field, state, or UI concept, search the glossary. Run `navigator-deploy glossary list` for term
titles and plain-language descriptions. Then run `navigator-deploy glossary show "<Term>"` for a term's full definition.
Use the glossary's term when one fits; when none fits, add a glossary entry in `docs/glossary/` in the same change
rather than inventing a synonym.

## The omnichannel inbox and the Person CRM

The inbox is a three-layer slice of the workspace: `store/src/inbox.rs` owns the PostgreSQL conversation, message,
ingress, and assignment accessors; `portal` owns the seams — `inbound_email.rs` receives SendGrid Inbound Parse,
archives the raw message, and hands it to the store, while `lawyer_mailbox.rs` keeps a lawyer's own outgoing address
from re-entering the queue as a counterparty; `webapp/src/inbox.rs` renders the Dioxus list, thread, and triage
surfaces. Inbound email resolves its sender to a Person (alias-aware, primary plus `person_mailbox`) at ingest and
persists that association on the conversation; unknown senders may create a Person row. Automation writes identity and
Relationship Log entries only — never a typed `relationship` or `entity_role` edge, and never a `person_mailbox` alias
row. Conflicts read the one firm-wide graph through `store/src/conflicts.rs`; email-derived identity lands there by
being a correct Person, not by minting edges.

## Email is draft-only

An agent never sends, replies to, or forwards email. Every email an agent touches ends as a draft for lawyer review —
the same doctrine the [Project-repo contract](cli/src/projects/agent_contract.md) states for Project repositories,
applied to this one. To send something, prepare the draft and hand it to the lawyer.

## Least code, coverage never lower

Write the least amount of code the behavior needs: delete dead code instead of annotating it, consolidate duplicated
helpers into the sanctioned seams, and name a test before the change that satisfies it. CI holds the workspace at a
92.2% line-coverage floor and that floor is never lowered — a subtraction lands only with coverage re-verified
(`docs/agent-workflows.md` explains the measurement).

## Helpful commands

### Reading the ontology

```bash
cargo run -p deploy -- glossary list
cargo run -p deploy -- glossary show "Lawyer Review"
```

### Reading the Project Skill catalog

A Project Skill is a jurisdiction/practice-area playbook a Project pins onto its own `navigator.yaml` — distinct from
the Agent Skills under `.agents/skills` this file just pointed you at. See
[`docs/project-skills.md`](docs/project-skills.md) and `glossary show "Project Skill"`.

```bash
cargo run -p cli -- project skill list
cargo run -p cli -- project skill show nv estates
```

### Starting web

```bash
cargo run -p deploy -- dev up --path "$PWD"
set -a; source .devx/env; set +a
cargo run -p neon
```

### Lifecycle commands

```bash
cargo run -p deploy -- dev down --path "$PWD"
```

### e2e Testing

Run if doing UI changes.

```bash
cargo run -p deploy -- dev browser-e2e
```

### Testing

Run the focused test while writing the change. Before pushing a pull request, run `navigator validate`, or the CLI crate
when `navigator` is not on `PATH`. Do not re-run the workspace suite at that step. CI runs it.

```bash
navigator validate
cargo run -p cli --quiet -- validate
```

### Local K8 Development

If we spin up local Kind, inspect workloads with:

```bash
kubectl --namespace navigator get pods
kubectl --namespace navigator describe pod <name>
kubectl logs --namespace navigator <name> --all-containers --tail=100
```

## The three sample matters

The fixture seeds three matters, each with its own client, its own practice, and its own declared Matter Pages:

| Code | Matter | Repository |
| --- | --- | --- |
| `sample-litigation` | *Cruller v. Prine* | `neon-law/sample-litigation` |
| `sample-transactional` | *Widget Works — Outside Counsel* | `neon-law/sample-transactional` |
| `sample-estate` | *Estate of Cornelius Montgomery* | `neon-law/sample-estate` |

Each GitHub repository is a public, read-only snapshot that local development seeds from, named for the Project code it
mounts on. Staging's source of truth is the staging Forgejo, under `shook-law-pc/<code>`.

## Use CLIs and MCPs over Computer Use

- `navigator` — our own app or the CLI target
- `gh` — GitHub CLI for PRs

## Cursor Cloud specific instructions

A Cursor Cloud Agent boots from [`.cursor/environment.json`](.cursor/environment.json), whose `install` runs
[`.cursor/install.sh`](.cursor/install.sh).

```bash
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0
export RUSTFLAGS="-C link-arg=-fuse-ld=lld -C strip=symbols"
navigator validate
```

When `navigator` is not on `PATH`, run the same check through the CLI crate: `cargo run -p cli --quiet -- validate`.

SurrealDB is the legacy compatibility lane: PostgreSQL is the storage authority for every new model (see
[`postgres`](.agents/skills/postgres/SKILL.md)). Start SurrealDB and set `NAVIGATOR_SURREAL_*` (root/root) only to run
the compatibility lane's tests; without it those tests self-skip.
