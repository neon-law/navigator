---
kind: workshop
title: Contributing to Neon Law Navigator
description: Five ways to make the Neon Law Navigator corpus better for the next lawyer — most need no code.
---

# Contributing to Neon Law Navigator

## Intro

### Five ways to contribute

Pick the path that fits the time and craft you have:

1. Improve the product repository (authorized lawyer).
2. Add a fillable government PDF.
3. Tell the team what you learned.
4. Join a workshop or presentation.
5. Use Neon Law Navigator and report what breaks.

---

Neon Law Navigator includes original work copyright Neon Law IP LLC and contributions copyright their holders. Its
first-party source is MIT OR Apache-2.0. This workshop covers the product repository: templates, government form
mappings, tests, and the shared corpus. Accepted contributions use the same license; contributors retain copyright and
no separate CLA is required. AI-assisted contributions have no mandatory human-review gate; see CONTRIBUTORS.md for
recommended practices.

The source is public at [neon-law/navigator](https://github.com/neon-law/navigator), so anyone can read, run, and fork
it. Outside pull requests are closed for now, so the ways below run through conversation and use rather than the merge
queue. Anyone may write to <support@neonlaw.com>.

## Improve the Corpus

### Contribute to the product repository

The whole stack is written in Rust in one monorepo, licensed MIT OR Apache-2.0. Outside pull requests are closed;
authorized lawyers contribute inside the firm. The most useful corpus contributions are specific ones:

- Add a missing legal template. Add a government PDF form. Improve a field map. Add a test that locks a workflow to the
  law it implements.

- Maintain a brand catalog. Each public brand keeps its English home and services copy in
  `neon/locales/en/<brand-key>`; the registered catalogs include `neon/` and `delete-your-data/`. A new catalog must be
  added through the brand registry with covering tests and attorney review for public legal copy.

---

The web app, CLI, and durable workflows share the MIT OR Apache-2.0 license. Authorized contributors can use the
[Operating Neon Law Navigator](/workshops/deploy-the-navigator) workshop or the local KIND loop. Accepted contributions
retain their authors' copyright and are licensed under MIT OR Apache-2.0 without a separate CLA. We recommend checking
copied material, AI-generated output, dependency terms, and notices. Merge does not promise review or correctness.
Outside pull requests remain closed for now; write to support@neonlaw.com with a contribution inquiry and use
SECURITY.md for private vulnerability reports.

**Scope is not the gate — the local KIND loop is.** A contribution may be as ambitious as the problem demands: a
sweeping refactor, a new service beside the existing ones, even replacing a subsystem. What every change must do is run
inside the local KIND cluster that `cargo run --release -p hudson -- dev up` stands up — the loop that already runs
PostgreSQL, Rauthy, SeaweedFS, and OpenObserve in one KIND dependency tier. A change that introduces a new dependency
wires it into that loop in the same pull request, with a KIND manifest beside the existing ones, so the whole stack
still comes up locally.

That rule keeps the local dependencies reproducible. PostgreSQL stores application data and DBOS state. Object storage
holds document bytes. Use the local stand-ins to test the application's behavior, then verify the selected deployment's
managed services separately. A laptop test does not prove cloud IAM, backups, or production availability. An authorized
contributor opens the pull request after the relevant checks pass.

### View content images locally

The repository deliberately leaves published blog, presentation, and workshop image bytes out of Git. Before running the
site locally, hydrate the images referenced by the content tree from the public asset route:

```bash
cargo run -p hudson -- ops assets fetch-referenced \
  --base-url https://www.neonlaw.com/assets
cargo run -p neon
```

This is a read-only HTTP download and needs neither Google Application Default Credentials nor access to the production
bucket. The bucket stays private: the running application reads only its marketing assets with its own workload identity
and publishes those bytes at `/assets/*`. Re-run the command when a post gains or changes an image; the files land in
the ignored `server/public/img/` tree and are served locally from `/public`.

---

Keep the two access boundaries distinct. The production bucket is not public and does not need to become public for a
developer to preview the blog: the public asset route reads only the marketing-image lane. `fetch-referenced` uses that
route, so it fetches the same bytes a visitor's browser can reach while keeping documents, exports, and logs out of the
path. The downloaded bytes remain local and ignored; they are a development cache, not repository content.

When creating a new slide image, keep the full-resolution PNG or JPEG at `server/public/img/<deck-slug>/<filename>` for
local preview. Upload that same relative key to staging first and then to production; publishing one bucket never
publishes the other. Production upload remains an authorized operator action.

### Publish presentation images

Save each finished PNG or JPEG under `server/public/img/<deck-slug>/` and reference it as `img/<deck-slug>/<filename>`
in the presentation. Run these commands from the Navigator repository root:

```bash
hudson ops assets upload --dir server/public/img --bucket ${NAVIGATOR_STAGING_DEPLOYMENT}-assets
hudson ops assets verify --base-url https://staging.neonlaw.com/assets
```

Publish to staging first. Each deployment has its own assets bucket.

---

Presentation image bytes stay out of Git. Keep the full-resolution local copy in the ignored image directory, and commit
the Markdown reference and accurate alt text. The image key must be identical locally and in each deployment:
`img/<deck-slug>/<filename>`. The tracked Neon Law logo at `/public/logo.svg` ships with the application and needs no
bucket upload.

`assets upload` uses Google Application Default Credentials. Its explicit `--bucket` selects the destination without
changing the active gcloud project. Upload to `${NAVIGATOR_STAGING_DEPLOYMENT}-assets`, confirm the objects' byte
lengths and hashes, and check the staging public asset origin before the authorized operator repeats the upload for the
other deployment's assets bucket. Publishing to staging does not publish to another bucket.

The upload command walks every recognized image below `--dir`. To publish just one presentation, prepare a temporary
directory containing only its `<deck-slug>/` folder and pass that temporary parent as `--dir`; passing the slug folder
itself would drop the slug from the uploaded keys. Keep the same filenames and relative paths for every deployment.

Uploading images does not deploy the presentation's Markdown or catalog entry. Those changes follow the pull-request and
release flow on the next slide. Verify the public asset origin after publication and after the application roll.

### Ship through GitOps

Every change travels through a pull request to `main`. The required checks and resolved review threads protect the
branch; auto-merge lands one squash commit, then the release and deployment flow begins from that reviewed tip.

---

GitOps keeps ordinary contributors out of settings drift. Branch protections and merge behavior live in the `navigator`
CLI, so an authorized operator reviews the planned change before applying it to one named repository:

```bash
hudson ops github setup neon-law/navigator --dry-run
```

The repository name is optional — it falls back to `GITHUB_REPOSITORY` and then this checkout's `origin` — but the
command still reconciles one repository at a time and cannot apply to every repository at once. The boundary is a
`(host, organization)` pair: the host from `NAVIGATOR_GIT_HOST`, defaulting to `github.com`, and two admissible
organizations — `neon-law` that holds Navigator itself, plus the deployment's own `NAVIGATOR_GITHUB_ORG` when one is
configured. A repository outside that pair is refused before a token is read. It reconciles pull-request-only and
squash-only policy, gated on one check named `ci`. Authorized contributors should read `docs/gitops.md` in their
checkout before changing the real setting; contributors normally work through a PR and let the gate do its job.

### Work with the store

PostgreSQL is the application store. `dev up` writes the worktree's connection contract into `.devx/env`; use that
environment for the selected checkout.

- PostgreSQL schema and migrations live under `store/postgres/`.
- New storage tests use the PostgreSQL fixture described in `docs/test-database.md`.

---

Read `docs/test-database.md` before adding a storage test, and follow the PostgreSQL skill for schema changes and
migrations. Keep authorization, isolation, persistence, and recovery covered.

### Add a fillable government PDF

Government forms are one of the best places to contribute because the path is concrete. A form-backed template lives
under `templates/forms/...`, names the official `origin_url`, asks its intake questions from the shared question bank in
`store/seeds/Question.yaml`, and maps answers onto the PDF's AcroForm fields through either a `.fields` manifest or a
`.fields.toml` map. The workflow still passes through `lawyer_review` before the filled packet can be signed or filed.

---

The Nevada LLC formation packet is the model to study. Its template,
`templates/forms/united_states/nevada/state/nv__llc_formation.md`, asks canonical states like `person__client`,
`entity__company`, `person__registered_agent`, `custom_single_choice__management_structure`, and
`people__managing_members`. Its re-authored field layer,
`templates/forms/united_states/nevada/state/nv__llc_formation.fields`, uses those same state paths as PDF field names.
That is the contract: the common question bank gives the questionnaire a stable vocabulary, the PDF layer uses that
vocabulary, and `forms/tests/question_code_contract.rs` fails if the two drift.

When you add a form, start from the real issuing-authority blank, run `navigator template forms fields <code>` to read
the field names off the exact bytes, map only the fields the questionnaire can answer, and leave payment-card or
lawyer-only acceptance fields unmapped. Then run the form tests before you open the issue or PR:

```bash
cargo test -p forms
cargo run -p cli -- project gate
```

## Share What You Learn

### Tell us what you learned

The most valuable thing you can send is what you learned using it: a template that worked, a checklist item we are
missing, a bug, a kaizen improvement — never a client's data, just the craft. Email support@neonlaw.org.

---

You do not need a GitHub account or a single line of code to make Neon Law Navigator better. The [Using Neon Law
Navigator](/workshops/use-the-navigator) workshop closes by asking you to send the markdown of the template you built
and the one kaizen improvement you found — that is this contribution. Every template a lawyer shares raises the floor of
competence for the next lawyer who joins. Send the craft, never a client's file: sharing a template or a checklist
offers it for inclusion under MIT OR Apache-2.0, subject to the contributor terms. Contributors retain their copyright;
submission does not assign ownership to Neon Law IP LLC. Check `CONTRIBUTORS.md` before sharing work owned by someone
else.

### Join a presentation

We give talks on how and why we build this. [Rust in Peace](/presentations/rust-in-peace), our Rust NYC talk, dissects
deterministic legal workflows, with source-attributed snippets checked against the repository.

---

Presentations go deeper into the engineering and the argument behind it. [Rust in Peace](/presentations/rust-in-peace)
walks the path from statute to Cucumber feature to template to notation, one attorney-gated step at a time. A build test
checks source-attributed snippets against their cited files; illustrative examples are labeled separately. More talks
are on the way. Come to one, push back on the design, and tell us where it breaks — that pressure is how the
architecture earns its keep.

## Put It to Work

### Use Neon Law Navigator

The simplest contribution is to use it. Every matter you run and every instance you stand up surfaces the next
improvement. Start with [Using](/workshops/use-the-navigator); run your own with
[Operating](/workshops/deploy-the-navigator).

---

Using the platform is not a lesser contribution — it is the one that generates all the others. A lawyer who runs a real
matter in [Using the Navigator Workshop](/workshops/use-the-navigator) finds the missing checklist item; an operator who
stands up an instance in [Operating Neon Law Navigator](/workshops/deploy-the-navigator) finds the rough edge in the
install. What you learn flows back through the four contributions above.

## Wrap Up

### Why this matters

A corpus the whole community improves is how routine legal work gets cheap enough to reach the people priced out of it
today.

---

Neon Law Navigator is published so that anyone can read it, learn from it, and build on it, and every fix and every
template compounds for the next clinic and the next small firm. That is the access-to-justice fight, and contributing —
in any of these five ways — is how you join it.
