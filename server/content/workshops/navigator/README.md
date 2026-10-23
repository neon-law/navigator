---
kind: workshop
title: Using Neon Law Navigator
description: Work one litigation matter end to end, from the retainer template to its Matter Pages.
---

# Using the Navigator Workshop

This workshop uses one local matter from start to finish: **Cruller v. Prine**. The matter code is `sample-litigation`,
its source repository is the [litigation sample project](https://github.com/neon-law/sample-litigation), and its local
data is synthetic. Every attendee sees the same matter through the role assigned to their local account.

The fixture seeds two more matters beside it — `sample-transactional` and `sample-estate` — so the participation-scoped
project list has something in it. The exercises stay on one matter on purpose; the other two are there to be seen from
`/app/projects`, not worked.

## Why do this

Today, a matter often lives across a practice-management suite, email, a document store, and a signature tool. The
client sees none of that coordination and calls for an update. The firm pays per seat for the glue between systems.

Navigator changes the work in four ways:

- **Same-day engagement**: a services page leads to a questionnaire, a drafted document, lawyer review, and a send in
  one system.
- **A bill the firm owns**: the firm runs the software with a repository per matter, rather than a per-seat licence.
- **Security by assignment**: participation gates every door, refusals are logged, and a lawyer approves before
  anything leaves.
- **A client surface per matter**: the matter's repository declares its Matter Pages.

You leave with a litigation matter walkthrough and a three-minute demo you can run at your firm. You can also identify
which outcome your firm would buy first and what it would need to see.

Navigator does not yet provide a client-facing case dashboard or timeline.

### Learning objectives

- **Remember** — identify Project, Template, Notation, Workflow, and the unique onboarding / offboarding pair.
- **Understand** — connect each noun to the database row that makes the workflow durable and inspectable.
- **Apply** — open the litigation matter, bind the shared retainer template, and view its Matter Pages.
- **Analyze** — inspect the notation state and the matter's participation-scoped views.
- **Evaluate**: identify which outcome your firm would buy first and what it would need to see.
- **Create** — make a small, testable change in the sample project and refresh the local matter.

---

Start by naming the four nouns. The workshop keeps every exercise on one matter so the room shares one durable
reference.

### The running matter

The local development fixture seeds three open Projects. This is the one the workshop works:

- **Name** — Cruller v. Prine
- **Code** — `sample-litigation`
- **Matter** — trespass to land, and rescission of the doughnut instrument
- **Repository** —
  [neon-law/sample-litigation](https://github.com/neon-law/sample-litigation)
- **Matter page** — `/app/projects/sample-litigation`

The other two seeded matters are `sample-transactional` (a company on a monthly retainer) and `sample-estate` (an estate
plan). Each carries its own repository and its own declared Matter Pages, shown the same way.

The Project code is the public URL key. Codes use lowercase letters and numbers separated by single hyphens, so a
project page is always readable as `/app/projects/<code>`.

---

Point out the code in the matter URL. It is the stable, human-readable project identity used throughout the exercise.

## Develop locally

### Start the local room

The same local `web` process resolves its public face from the host. In staging, use `staging.neonlaw.com` for Neon Law,
`staging.deleteyourdata.com` for DeleteYourData.com, and `staging.lawyershook.com` for Lawyer Shook. The route and
application stay shared; the host selects the brand's copy, mark, font, and colour layer.

The Navigator CLI owns the complete local lifecycle. From a New Worktree, run:

```bash
cargo run -p cli -- dev worktree-env up --path "$PWD"
set -a; source .devx/env; set +a
cargo run -p neon
```

The boot command provisions the KIND dependency tier, applies the schema, seeds the sample matters, clones each sample
project, stages its `navigator.yaml`, `drafts/`, and commit, and writes the generated environment. The host web process
reads that environment on startup, so each matter's declared pages render on its show page after each boot.

The explicit refresh command uses the same clone and staging path when a sample project changes. Naming one matter
refreshes only that matter, which is the fast loop while iterating on a single repository:

```bash
cargo run -p cli -- dev sample-project --project sample-litigation
```

Restart `web` after refreshing so it reads the new staged source. The generated `.devx/env` contains
`NAVIGATOR_SAMPLE_PROJECTS_DIR`, the directory every matter's source is staged under; source it before starting the host
process.

---

Run the boot commands before proceeding. Confirm that `.devx/env` names the staged sample-projects directory.

### Sign in

The local Rauthy fixture supplies five role-named accounts, all using the password `password`:

| Account | Role | Matter access |
| --- | --- | --- |
| `owner@neonlaw.com` | owner | firm-side matter view |
| `admin@neonlaw.com` | admin | administration surface; participation can be granted there |
| `lawyer@neonlaw.com` | lawyer | firm-side matter view |
| `clerk@neonlaw.com` | clerk | supervised matter view |
| `client@neonlaw.com` | client | client matter view and Matter Pages |

Open `$NAV_BASE_URL/auth/login`. Firm accounts land on `/app/team`; the client account lands on `/app/projects`. The
project list and detail page use `sample-litigation` in the URL. The matter show page is available at:

```text
$NAV_BASE_URL/app/projects/sample-litigation
```

The matter page is participation-scoped. The client, lawyer, clerk, and owner rows are part of the fixture, and the
admin account is the local administrator used to exercise the participation controls at `/app/admin`.

---

Have each attendee sign in with the role relevant to their work. Keep the browser on the litigation detail page.

## Work the litigation matter

### The four nouns in one workflow

1. **Project** — Cruller v. Prine, the matter that owns the work.
2. **Template** — a versioned Markdown blueprint such as `onboarding__letter`.
3. **Notation** — one client and one Template bound inside the Project.
4. **Workflow** — the states and transitions that move a Notation from intake through review and signature.

The shared retainer template is available in the canonical catalog. A lawyer can bind it through the Navigator MCP
catalog:

```text
create_notation(template_code="onboarding__letter", project_id=<sample-litigation project id>)
```

The notation begins in its seeded workflow state. The lawyer reviews the generated work, advances the workflow through
the configured transitions, and the resulting documents remain tied to that Project and its audit trail.

---

Trace one notation from its template through its workflow state. Relate each step back to the same Project.

### One onboarding, one offboarding

Every Project has **one** onboarding notation and **one** offboarding notation. Those two kinds are unique on the
matter: a second retainer is not a second engagement, and a demand letter is not a closing letter.

Onboarding is the letter that opens the matter. Offboarding is the firm-signed closing letter that ends the
representation. Those two shared catalog codes are `onboarding__letter` and `offboarding__letter`. Opening the Project
does not create either notation; a lawyer binds them like any other template. A pitch with no prior notation may start
with its offboarding letter so the lawyer can close it; other first notations must open the matter.

The CLI seeds both letters. This workshop still binds the retainer through Navigator MCP as one onboarding walk. Close
the matter with the offboarding letter. Do not bind two onboardings on one Project:

```bash
navigator site notation create onboarding__letter \
  --project sample-litigation \
  --client-email client@neonlaw.com
navigator site notation create offboarding__letter \
  --project sample-litigation \
  --client-email client@neonlaw.com
```

---

Name the pair on Cruller v. Prine. The onboarding letter is `onboarding__letter`. The closing letter is
`offboarding__letter`. The Projects-list badge reads presence — `onboarding on file` — not execution. A bespoke letter
still counts if it declares `kind: onboarding` or `kind: offboarding`.

### Walk the retainer intake one question at a time

Navigator MCP binds the retainer in one call, but a lawyer can also walk it by hand at
`/app/lawyer/notations/{notation_id}/step` — one question per screen, the same focus-set chrome the client's own
self-serve intake shares: a step list naming the whole chain, a progress bar, and the question's own control below it. A
closed choice — the engagement's governing law, or a yes/no — renders as cards to choose between, not a bare checkbox or
a compact radio list. Completed steps show as plain markers; there is no revisit route today.

The client-facing half of the same chain is `/app/projects/{code}/intake/{notation_id}` — the client confirms or
corrects whatever the lawyer already entered, through the identical chrome.

---

Start a retainer walk from `/app/lawyer/retainers/new` and step through a few questions live. Point out that the step
list names every question up front, not just the current one, and that the governing-law question at the end offers its
three choices as cards.

### The other notations on a matter

After the engagement is on file, the matter accumulates the work itself. Those later notations are not unique. A
litigation matter may carry many letters and filings. An estate matter may carry a will, a trust, and directives. A
review matter may carry a memo.

The vocabulary is one closed enum, `Kind`, in `rules/src/kind.rs`. A template declares `kind:` in its frontmatter.
Generated PDFs and lawyer uploads reuse the same strings on the asset lane. The notation kinds you add after onboarding:

- `letter` — a letter the firm sends on the client's behalf (demand, notice, settlement)
- `filing` — a document filed with a government body
- `will`, `trust`, `directive` — estate instruments
- `agreement` — a private agreement with a third party
- `pleading` — court paper filed with a court (complaint, motion, brief)
- `memo` — an analytical work product, not an executed instrument

Filed uploads that are not templates use `transcript`, `inbound_contract`, `certificate_of_naturalization`, or
`unclassified`. Content pages (`post`, `workshop`, `event`) and matter dashboards are not notations on the Project.

---

Walk the list against this litigation matter. The retainer is onboarding. A later demand letter is `letter` and may be
one of several. Do not treat `letter` as offboarding.

### Inspect the matter page and its Matter Pages

Sign in as `client@neonlaw.com`, open `/app/projects`, and select **Cruller v. Prine**. All three seeded matters are in
that list, because the fixture client participates in each one. The detail page keeps the human-readable code in the
address bar:

```text
/app/projects/sample-litigation
```

The show page renders the Matter Pages the sample repository declares under `pages:` in `navigator.yaml`. Each page sits
under the Project's code, which gives the sample a complete path from repository to matter-specific browser surface.

---

Ask the room to identify what the client can see from this page and what the firm-side matter view adds for the team.

### Make a sample-project change

The sample repository declares its Navigator Project in `navigator.yaml`:

```yaml
version: 26.10.17
project:
  name: sample-litigation
  host: staging.example.test
```

This is the manifest shape. Keep the checkout's own pinned version and deployment host when doing the exercise.

Edit the sample project, run the refresh command, restart `web`, and reload the matter URL. Boot validates the manifest,
stages the source, and stores the declared `pages:` against the declared Project.

That manifest is the whole reason three staged repositories cannot collide. Each is staged in a directory named for its
matter, but boot re-reads the manifest rather than trusting the directory, and refuses a checkout naming a different
Project — because accepting one would put one client's pages on another client's matter.

---

Make one small change to a declared page, refresh, and show the reloaded matter page. The manifest name remains
`sample-litigation`.

## Use the Project's forge

### Sign in, then receive repository access

Forgejo hosts source repositories and pull requests. Open the repository recorded on the Project rather than guessing
its host. During migration, an existing Project can still point to GitHub.

On an installation with Forgejo admission configured, sign in through its identity provider with a verified address in
an allowed domain, then enroll two-factor authentication. Registration creates an account; repository access is a
separate grant. Navigator Project participation and Forgejo repository permissions are separate checks.

The deployment template keeps Git access on HTTPS and SSH disabled. Short-lived SSH certificates are an access design to
prove before enabling SSH; they are not a prerequisite for reading a pull request in the browser.

---

Open the synthetic Project's repository link. Identify its forge and confirm the signed-in account has explicit access
to that private repository. Keep the local Navigator fixture login separate from the forge's sign-in flow.

### Send a change through the same gate

Make a branch and open a pull request. Read `navigator.yaml`: its version pins the CLI, `project.name` identifies the
Project, and `project.host` names its deployment. Then run:

```bash
navigator project gate --ci
```

A Forgejo cutover requires a trusted required check for the current commit, protected `main`, and PR-only squash
merging. A missing, skipped, or failed gate must block merge. Repository access does not grant a direct push to `main`
or permission to change its protection.

---

Use a synthetic branch to inspect a failing gate and its explanation. Correct the input and inspect the successful
rerun. Ask the operator for evidence of the installed merge policy; a copied repository alone does not prove cutover.

### Follow the gate and its status

The existing GitHub workflow reports main-push gate outcomes to the Project's private Slack channel. The Forgejo
migration must prove that the same gate and notification paths work before switching the Project.

The required notification flow includes failures to read `navigator.yaml`, failed checks, and recovery. It must resolve
the channel from Navigator's trusted repository binding so a broken manifest cannot silence the alert. These are cutover
requirements, not a claim that Forgejo notifications are already installed.

Document YAML points to Navigator-managed objects. Keep legal-document bytes in that object store rather than adding
them to Git or LFS. Gate success is the outcome to inspect.

---

Find the gate result and the Project channel. In the Forgejo pilot, record any missing link as an incomplete migration
step. Do not upload a legal document to a pull request or paste it into a notification.

## Open a real Project

Every exercise so far works the one seeded matter. Opening a *new* Project is a different, less-worked path — walk it
once so the room can tell "the fixture already did this" apart from what a lawyer actually does to start a real
engagement.

### Fill the matter-open form

A lawyer-tier account reaches the form at `/app/projects/new`. It asks for a name, a code, an Entity, a description,
scope of services, the client DRI, and a required conflict-check attestation. Navigator stores the code exactly as typed
— the code is immutable, and a code already in use by another matter is refused rather than silently resolved, because
it is a coordinate the caller already committed to elsewhere (a repository's `navigator.yaml`, a Drive folder name).

---

Fill the form live against a throwaway name so the room sees every required field before it submits. Do not open a real
matter in the shared room database.

### Provision the repository and Drive folder — a separate step today

The browser form does not provision the Project's repository or Drive folder by itself. Every other way of opening a
Project — the JSON API, the CLI, the MCP tool — provisions both surfaces as part of opening; the browser form only
writes the matter row. Immediately after submitting, `/app/projects/<code>` names no repository yet.

Close that gap from the matter page's admin retry, or from the CLI:

```bash
navigator project setup <code>
```

This creates — or adopts, if one already exists — an empty private GitHub repository and Drive folder, and records the
repository URL on the row. It writes no files.

---

Run `reconcile` live and point out the row had no repository URL until this step. Contrast it with the JSON API, CLI,
and MCP paths, which provision both surfaces synchronously as part of opening.

### Populate the repository

Clone the now-existing empty repository, then build its shell by hand: copy `AGENTS.md` and `.agents/skills/` from
Navigator's own repository root, write `README.md` and a nested `navigator.yaml` naming this Project and host, and add a
starting `templates/<code>.md`. Then reconcile the two generated workflows — `.github/workflows/ci.yml` (the thin CI
caller) and `.github/workflows/cd.yml` (the main-push caller):

```bash
navigator ops github setup <owner>/<code> --action-version <YY.M.D>
```

This writes no application; the matter's pages are declared in `navigator.yaml`. Pass `--action-version` explicitly
rather than relying on a default, which only resolves from a real release build.

Commit and push. That push is what makes the CI gate live on the new repository.

---

Build the shell against the freshly cloned repository and show the room the files it carries. Commit and push before
moving on — the CI gate does not exist on the repository until that push lands.

### Stage a document and sync it

The repository shell above does not write `documents/` or `blobs/`. They are the two root-level document folders in
every Project repository, alongside `templates/`. `blobs/` holds one committed YAML pointer per document, and
`documents/` holds the bytes, staged for upload or pulled down for reading; the root `.gitignore` keeps the whole of
`documents/` out of Git. Neither is a second document store. Git keeps the pointer; Navigator keeps the bytes. Drop a
local file below `documents/`, list what a sync would do, then run it:

```bash
navigator project sync --dry-run
navigator project sync
navigator project gate
```

Against a staged `documents/exhibits/exhibit-a.png`, the dry run prints one line per staged file and a count, and
changes nothing:

```text
would upload documents/exhibits/exhibit-a.png
1 upload planned
```

The real run derives the document slug from its path below `documents/`, uploads the bytes through Navigator's
authenticated API, and writes `blobs/exhibits/exhibit-a.png.yaml` only after the upload succeeds. The pointer records
`kind`, desired `visibility`, current revision metadata, and the previous asset id when the chain has one. It never
contains an object-storage coordinate or legal-document bytes; object storage and the `assets` revision chain remain
authoritative. Folder conventions infer `filing` for `pleadings/`, `exhibit` for `exhibits/`, and `agreement` for
`agreements/`; other paths use `unclassified`. Visibility defaults to `internal`.

The real run needs a stored login — `navigator site login --host <host-or-url>` — and a matter with that code visible to
the account. `--dry-run` needs neither. The root `.gitignore` ignores `/documents/`, but an ignore rule only keeps
untracked files out of a plain `git add`; the gate is the control. `navigator project gate` rejects every tracked file
below `documents/`, and every file below `blobs/` that is not a `*.yaml` pointer:

```text
./documents/exhibits/exhibit-a.png: error: `documents/` is Git-ignored: legal documents and raw document bytes must not be committed, and each document's `*.yaml` pointer belongs under `blobs/`
```

---

Stage one throwaway file under a synthetic `acme` checkout and run the dry run first, so the room sees the plan before
anything moves. Run the real sync only against a matter the signed-in account can see; the seeded sample matters work.
Open the emitted `.yaml` under `blobs/` and point out what it does not contain. Finish by force-adding a raw file on
purpose (`git add -f`) and running `navigator project gate` so the room meets the gate error here, with context, rather
than on their first real upload.

### Pull a fresh checkout's documents

`navigator project sync` also fills a fresh clone: it downloads each committed pointer's own revision into
`documents/<slug>`, and discovers live documents the checkout has no pointer for. A fresh clone carries the pointers
under `blobs/` but no bytes; sync is what fills them back in without a manual `document get` per pointer.

```bash
navigator project sync --dry-run
navigator project sync
```

The dry run compares each pointer's recorded `sha256` against the local file — no login, no network call — and lists
what would change:

```text
would pull documents/exhibits/exhibit-a.png
1 pull(s) planned
```

The real run downloads through the same authenticated API `sync` uses, verifies the bytes against the pointer's own
`sha256`, and writes them to the staged path. A file whose digest already matches is left alone, so running `pull` again
after a full checkout prints `0 pulled`. It hydrates only: a live document the checkout carries no pointer for is
`sync`'s lane, not `pull`'s. A pointer the signed-in account cannot read is reported, not silently skipped.

`pull` is all-or-nothing for document bytes. It first downloads every missing or stale revision into a task-owned
temporary staging area, then publishes the staged files only after every pointer is present, authorized, downloaded, and
verified against its `sha256`. If a pointer vanishes, access is denied, a download fails, or a digest mismatches, the
command fails without changing any pre-existing target bytes and without creating any newly hydrated target. Writing a
missing root `.gitignore` is outside the document-byte guarantee. Correct the failure and rerun `pull`: a successful run
hydrates the complete set, and the next run reports `0 pulled`.

---

Delete the exhibit `sync` just uploaded, run `pull --dry-run` so the room sees the plan, then the real `pull` and show
the restored bytes are identical. Run `pull` a second time and point out the `0 pulled` — the digest check, not a
re-download, is what makes a repeated `pull` cheap.

### Matter Pages are a separate, later decision

Not every Project needs client-facing pages. When one does, it declares them under `pages:` in `navigator.yaml`, and
`navigator project pages init` prints a proposed block. The matter show page renders them; there is no application to
build.

---

Skip declaring pages for this walkthrough. Name `navigator project pages init` as the step a real Project would use once
one is actually needed.

### Verify

```bash
navigator project doctor --project <code>
navigator project drift --dir ~/neon-law
```

`doctor` reports whether this machine and this Project's coordinates agree — the repository URL and the Drive folder.
`drift` reconciles a machine's checkouts against every live Project row.

---

*Walk the sequence once against a throwaway code, pointing out exactly where the browser form stops and the CLI takes
over. Do not open a real matter in the shared room database — this is a narration, not a room exercise.*

## Wrap Up

### Verify the room

Run the browser and accessibility gate against the sourced environment:

```bash
cargo run -p cli -- dev browser-e2e
```

The gate signs in the local personas, checks the matter surfaces, and exercises the real local browser path. The Rust
suite and feature walkthrough cover the same fixture and its seeded participation rows:

```bash
cargo nextest run --workspace && cargo test -p features
```

The workshop is ready when the litigation matter appears in the intended role view, `/app/projects/sample-litigation` is
the detail URL, and that page renders the sample repository's declared Matter Pages.

---

Close by verifying the same matter path together. The seeded matter, development flow, and browser proof all meet at one
URL.
