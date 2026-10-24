# Gate

`navigator project gate` is the command every editor, CI gate, and this repository's `AGENTS.md` point at for a
recognised repository. This page is the canonical reference for the rule set both that command and `navigator validate`
share: what it runs, the error/warning split, and one row per rule code. `cli/tests/gate_docs_coverage.rs` fails the
build when a code exists in `rules/src/` or `cli/src/main.rs` with no entry here, so this table cannot go stale.

## Usage

A Navigator checkout and a Project repository run:

```bash
cargo run -p cli --quiet -- project gate
```

It takes no path. The gate runs on a whole repository, and it finds that repository by the `README` and the `.git`
beside it in the directory it was started from; anywhere else it refuses (exit `2`) rather than reporting a clean scan
over the files it never read. Run it from the root.

`--check` compares committed document pointers with the live record. The host and Project code come from
`navigator.yaml`. It moves a legacy layout's pointers from `documents/` to `blobs/` (rule `Y014`), rewrites a drifted
pointer, and writes a missing pointer under `blobs/`. It never writes to the live site. A missing or corrupt object
needs a person: `navigator site document repair` restores a missing object from a same-hash sibling in the matter. A
live row with no slug needs a person: `navigator site document slug` sets the slug and, when passed, the kind. A kind
the asset lane rejects needs a person: `navigator site document kind` replaces it. `--deep` re-hashes each object, and
offline hashes the Git-ignored bytes each pointer names under `documents/` against its `sha256` and `size_bytes`: each
pointer is reported `match`, `modified locally` (fails the run), or `missing locally` (passes, since a checkout need not
hold the bytes). Under `--ci` any fix this would make fails the job and the output names the fix. Without `--check`,
`project gate` makes no document request.

A directory that is neither of those shapes — no `Cargo.toml`, no `navigator.yaml`, no assumption about the surrounding
repository — still has the same rule set through `navigator validate [DIR]`. The directory defaults to `.`. `--fix`
writes every safe-by-construction edit and `--errors-only` hides Warning-severity advisories. `--ci` is still accepted
and changes nothing: it once held a built-bundle scan to a tree CI had built.

```bash
navigator validate
navigator validate /path/to/tree
```

Safe-by-construction fixes land as it goes — trailing whitespace, ATX heading spacing, blockquote spacing, and `S102`
paragraph packing — and each file is re-scanned until it stops changing, because one fix routinely uncovers another:
trimming trailing whitespace off a short line hands that line to `S102`, which could not flag it while it still looked
like a hard break. What remains is what a human has to resolve. `project gate` always writes those edits; `validate`
writes them only with `--fix`.

The walk covers authored content only. It descends into everything except the trees nobody authors: `.git/`, `target/`,
`.worktrees/`, `node_modules/`, and `dist/`, plus — for the Markdown passes — every other hidden directory apart from
the canonical `.agents/` skill catalog. The match is on a whole directory name, so `distributions/` and
`node_modules_policy/` are authored trees and stay in the gate. No pass reads a build: a Project repository commits only
source.

This is also the exact command every Project repository's generated CI gate runs against its own tree — see
[`project-repositories.md`](project-repositories.md) for the thin `ci.yml`/`cd.yml` callers a Project repository carries
and how `ops github setup` reconciles them. On a Project repository, the same run also closes `.github/` to exactly
`.github/CODEOWNERS`, the two thin workflow callers `.github/workflows/ci.yml` and `.github/workflows/cd.yml`, and
`.github/workflows/automerge.yml`: any other path there is a finding naming the exact path and the closed set it fell
outside of. Each caller is checked structurally against its canonical generator — permitted trigger, permissions, jobs,
`needs:` dependency between them, and the `project`/`host` inputs pinned release it calls — so a caller can differ from
the generator in whitespace, quoting, or key order and still pass, but not in which event triggers it, what it can do
with its token, or how many jobs answer for the required check. `automerge.yml` is checked byte-exact instead, the same
way `.github/CODEOWNERS` is: it is machine-owned, so any difference from the canonical copy is drift rather than local
intent, and `navigator project gate` (not `--ci`) writes the canonical copy over a missing or drifted one rather than
only reporting it.

`.github/workflows/gate.yml` and `.github/workflows/publish.yml`, the filenames `ci.yml` and `cd.yml` replaced, are
still read under those names through Navigator CLI release **26.9.23**: the gate accepts either one with a warning
naming the file to rename. Every release after 26.9.23 refuses the retired name outright — the constant naming this
bound is `FINAL_RETIRED_WORKFLOW_RELEASE` in `cli/src/projects/repository.rs`. Separately from that bound, a retired
file is always refused the moment its canonical replacement is also present — `ci.yml` and `gate.yml` (or `cd.yml` and
`publish.yml`) side by side is a repository where GitHub still runs the retired workflow while the gate validates only
the canonical one, so the finding names both paths and fails in every release, not only past the bound.

## What it runs

Nine normal validation passes happen in this order:

1. **The classified rule engine** (`rules::navigator_classified_rules_with_codes`) walks every `.md` file, classifies
   each one by its declared `kind:` (notation template, event, blog post, workshop, GitHub notation, or plain prose),
   and lints it against that kind's rule set — the S, N, E, C, and M families below.
2. **Cross-file code uniqueness** (`rules::code_uniqueness_violations`, rule `N111`) walks the same tree a second time
   and fails if two notation templates have the same filename stem. This runs after step 1 because it needs every
   template path before it can compare them.
3. **A YAML syntax pass** parses every `.yaml`/`.yml` file under `dir` and fails on a parse error. This has no rule
   code — it is a raw syntax check, not a lint — and it is not limited to notation templates or seed documents; any
   malformed YAML anywhere under `dir` fails it.
4. **A seed-document pass** (rule `Y001`) additionally validates every YAML file whose parent directory is literally
   named `seeds/` against `store::seed::validate_yaml`, the same shape check `navigator site import` enforces at write
   time. A seed document names real people and entities for a production write, so this pass exists to catch a malformed
   seed before it ever reaches `site import`.
5. **A locale-catalog pass** (rule `Y002`) additionally validates every YAML file under a `locales/<locale>/` directory
   against the typed marketing-copy schema in `views::locales`. The site publishes English only: a directory other than
   `en`, an unknown page stem, an unknown brand-key directory, or a document that does not deserialize as the page it
   names fails the gate. A house-of-brands tree uses `locales/en/<brand-key>/<page>.yaml`; a fixture may still use the
   flat `locales/en/<page>.yaml` layout. This is what lets a copy-only edit stay a YAML change without landing a catalog
   the brand crate cannot load.
6. **A document-pointer pass** (rule `Y003`) validates `blobs/**/*.yaml` (and the retired `.yml`) only when the root is
   a Project repository declared by `navigator.yaml`. It checks the closed asset kind and visibility vocabularies,
   current revision metadata, revision-chain linkage, and the retained document extension without reading the network or
   bytes. The same Project-repository check refuses a pointer still under the Git-ignored `documents/`, or the retired
   `documents/.gitignore` (rule `Y014`). Local `project gate` moves each pointer to `blobs/` and removes that file once
   the root `.gitignore` ignores `/documents/`; `--ci` reports it as a warning for one release (LAW-189).
7. **A Project-manifest pass** (rules `Y004`–`Y008`, `Y011`–`Y013`, `Y015`–`Y016`, `Y019`–`Y021`, and `D001`–`D005`)
   runs when the root carries either manifest spelling. It accepts the versioned nested Project shape, holds `host` to a
   hostname and `project.name` to `store::projects::is_valid_code`. It requires non-placeholder DRI emails and private
   Slack/Notion URLs, and checks optional handle shapes. `no_live_row` requires a reason; unknown keys and comments
   fail. Record reasons in PRs and repository contracts, and rename `navigator.yml` to `navigator.yaml` before
   validation; flat manifests stay deprecated with a warning during migration. Every `skills:` pin must carry non-empty
   `jurisdiction`, `practice_area`, and `version` (`Y015`); `navigator project skill use` records the pin. Catalog
   resolution is checked separately by `navigator project gate --check` because only the CLI binary carries the catalog.
   A `pages:` block is held to its shape (`Y021`), and each entry's composition to the D-family.
8. **A consumed mutable-tag pass** walks YAML files and Containerfiles/Dockerfiles for an image or binary reference
   pinned to a mutable tag (`latest`, a branch name) rather than a digest or release version, and fails on each one
   found. This has no rule code either.
9. **A public snapshot pass** (rule `P001`) runs only in a tree that carries `.github/public-snapshot/allowlist.toml`.
   It reads every file `git ls-files` tracks and refuses credentials, non-reserved email addresses, dialable phone
   numbers, Google Cloud coordinates, secret-bearing files, build output, and embedded image or PDF metadata unless an
   allowlist entry permits the finding. See [`public-snapshot-audit.md`](public-snapshot-audit.md).

The autofix runs inside pass 1 rather than beside it: each file is fixed and then linted, so what pass 1 reports is what
survived its own fixes. It is the same fix the `navigator-lsp` `source.fixAll` editor action ships.

## The one flag

**`--ci`** holds the run to what CI can prove, and it is what every CI job passes.

- **Nothing is written.** A file the gate would have fixed becomes an `F001` finding instead. A CI checkout is discarded
  when the job ends, so a silent rewrite there would pass a gate while leaving the unformatted file on `main` — the
  problem would never converge. Locally the fix simply lands and the run moves on.
- **The live Project check is read-only.** Locally, the gate uses the bearer from `navigator site login`; in CI, it
  exchanges GitHub Actions OIDC at `POST /auth/ci/document-token` on a push or dispatch to `refs/heads/main` or a pull
  request merge ref. It checks the live Project row and the required DRI, GitHub, Slack, and Notion coordinates against
  `navigator.yaml`. The host is the one the manifest declares. If the session cannot be authorized or any read fails,
  the gate fails as inconclusive; it never treats an unavailable live check as a pass.
- **Each declared section's live data is counted (`Y023`).** For every section `pages:` declares, the gate asks the
  deployment with one `GET /app/api/projects/{id}/section-counts`, returning three nonnegative integers:
  `deadline_board` and `key_dates_strip` read open Deadlines, `download_list` reads Documents, and `docket_timeline`
  reads Docket Entries. A source with no rows is a `Y023` warning at the line naming the section, with the command that
  adds one. A brand-new matter legitimately has an empty timeline, and every section renders an empty state, so `Y023`
  is a warning on every event and never fails the run. Refused, failed, missing, or malformed counts print an
  inconclusive `Y023` warning. A CI document session reaches only its own Project counts; document filenames, deadline
  titles, docket content, and foreign Project counts remain outside its scope.
- **A pull request's closing trailer is checked against its own body.** On a `pull_request` event the gate reads
  `pull_request.body` from the runner's `GITHUB_EVENT_PATH`. A body that closes an issue (`Closes ENG-12`) while a
  sentence outside code and quotes says a rollout, an apply, or a caller pin is still pending fails the run. The finding
  names the issues and the sentence, and says to use `Refs` with the remaining step and its owner. No issue tracker is
  queried, so the check needs no network and no secret.

## Errors versus warnings

A rule's severity is either `Error` or `Warning`. An Error-severity violation, a YAML parse failure, a seed-document
failure, a locale-catalog failure, or a consumed mutable tag all fail the gate (exit code `1`). A Warning-severity
violation prints alongside everything else but never fails the run — it is a heads-up, not a blocker. Only four codes
are `Warning`: `N112` (a workflow step is allowed but its automation is not built yet), `M061` (a relative docs link the
renderer cannot map onto a site route or GitHub), `Y013` (the legacy flat Project-manifest shape), and `Y023` (a
declared section whose live data source is empty). Every other code, including `Y001` and `Y002`, is `Error`.

Every rule-backed finding in the primary listing opens with `error:` or `warning:`, the way `rustc` and `clippy` write
one, before the `path:line`, the rule code, and the message. The raw YAML-syntax and consumed-tag passes retain their
plain stderr diagnostics; the error recapitulation below renders those failures with `error:` too:

```text
warning: docs/example.md:12 M061: Relative link `lib.rs` renders verbatim on the website …
error: docs/example.md:104 S101: Line is 130 characters (max 120)
```

## The error recapitulation

A run that found any error closes with an errors-only block naming every failing line again, after all ten passes have
printed:

```text
2 error(s) fail this run:
error: docs/example.md:104 S101: Line is 130 characters (max 120)
error: locales/xx/home.yaml:1 Y002: locale directory `xx` is not published; only `en` is allowed
```

It is a separate block rather than a reordering because the standalone passes print *after* the markdown lint's summary
line, so no ordering within a single pass could gather a YAML error and a mutable-tag error together. Being additive, it
also leaves the primary listing in tree order — per pass, per file, per line — so a file's findings stay adjacent.
Reading it is how to answer "which line do I fix"; the summary counts and the exit code say only *how many*.

## Rule codes

Every code below is defined in `rules/src/`, except `Y001`–`Y023`, `F001`, and `P001`, which live in `cli/src/` because
the typed YAML, Project-manifest, origin, formatting, and public snapshot passes run outside the `rules` crate entirely.
"Autofix" means the gate rewrites the file for that violation without a human decision; every other code needs a person
to resolve it.

### S-family — cross-cutting structure

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `S101` | Error | A line exceeds the 120-character limit. | No |
| `S102` | Error | A line could absorb more text from the next line before hitting the limit. | Yes |
| `S103` | Error | The declared `kind:` must be a recognized document kind, and never a matter page kind. | No |
| `S104` | Error | A file's declared `kind:` must agree with its notation/event structure. | No |

`S102` runs on notation templates as well as prose, because a template renders into an instrument a client signs; the
rule's own guards hold back the structured YAML. The single exception is a GitHub intake notation under
`templates/github/`, which renders into an issue body rather than a document and keeps its prose inline with structured
YAML.

It reflows prose, so it holds back the block-level constructs whose lines carry meaning: headings, tables, block quotes,
fences, horizontal rules, setext underlines, link-reference definitions, and HTML blocks. It recognises the last two the
way CommonMark does. A definition needs at most three spaces of indentation, a label free of unescaped brackets, a
colon, a destination that is bare-and-unspaced or wrapped in `<…>`, and then either nothing or a complete title; a title
on the next line belongs to a definition that did not already carry one. An HTML block needs one of CommonMark's seven
start conditions, which means a block-level tag name or a complete tag standing alone on its line. Anything looser is
prose, so `[text]: this is prose` and a paragraph opening `<span>inline</span>` reflow like the sentences they are. Two
of those guards are what make the rule safe on a legal template: frontmatter is reflowed only inside a folded (`>`)
scalar, never a literal (`|`) one or a plain mapping, and a line ending in a hard break — two trailing spaces or a
trailing backslash — is left alone, which is what a signature block is built from.

### N-family — notation template shape

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `N101` | Error | Notation template must declare a non-empty `title`. | No |
| `N102` | Error | Notation template must declare a valid `respondent_type`. | No |
| `N103` | Error | Notation template filename must be snake_case. | No |
| `N104` | Error | Questionnaire/workflow state references an unknown registry item. | No |
| `N105` | Error | Notation template must declare `confidential`. | No |
| `N106` | Error | Notation workflow must include a `lawyer_review` step. | No |
| `N107` | Error | Signature placeholders must match the declared signer set and signing state. | No |
| `N108` | Error | Filename stem supplies the code; a frontmatter `code:` autofixes away. | Yes |
| `N109` | Error | `output:` must name a known render format, and its paired keys must travel with it. | No |
| `N110` | Error | Catalog: `notations/` shelves. Project: flat `templates/<code>.md` or `drafts/<code>.md`. | No |
| `N111` | Error | Notation template filename stem must be unique across the whole tree. | No |
| `N112` | **Warning** | A workflow step is allowed but its automation is not built yet. | No |
| `N113` | Error | Questionnaire state type must be a registered question type. | No |
| `N114` | Error | A `__for_` child state must follow a role-matched person/entity parent. | No |
| `N115` | Error | Data paths, iterators, and declared signer roles must resolve against questionnaire state. | No |
| `N116` | Error | Notation workflow must gate every outbound submission behind lawyer review. | No |
| `N117` | Error | A `custom_text__*` state the respondent answers must be an allowlisted free-text primitive. | No |
| `N118` | Error | Questionnaire must be one linear chain from `BEGIN` to `END`. | No |
| `N119` | Error | A `kind: github` notation must be one of the two shelf paths and ask its required questions. | No |
| `N120` | Error | A template body placeholder must name a declared questionnaire state. | No |
| `N121` | Error | A `sent_for_signature` state must be preceded by a `generate_pdf` state. | No |
| `N122` | Error | Every declared questionnaire state must be read by the template body. | No |
| `N123` | Error | An outlined kind's body must be a Harvard outline, titled to match its frame. | No |
| `N124` | Error | A services catalog template reference must name a notation under `templates/notations/`. | No |
| `N125` | Error | A subsection under a numbered section must be a lettered block quote. | No |
| `N126` | Error | A `skills/` catalog entry's frontmatter must parse and declare a seeded jurisdiction. | No |
| `N127` | Error | Project Skill catalog entries must have a unique `(jurisdiction, practice_area)` pair. | No |
| `N128` | Error | Only a form notation (`notations/forms/` or `output: form`) may declare `origin_url:`. | No |
| `N129` | Error | A notation's `audiences:` may only narrow its `custom_*` questions. | No |
| `N130` | Error | A pleading body carries no caption markup; the caption comes from `matter:` and frontmatter. | No |
| `N131` | Error | A template carries no drafting marker: bracket, stray blank, note, or typed Signer Box marker. | No |
| `N132` | Error | Clause references pin a known version, match its kind, and bind all inputs. | No |

### E-family — events

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `E001` | Error | Event must declare both a `starts_at` timestamp and a `timezone`. | No |
| `E002` | Error | A file is either an event or a notation template, never both. | No |
| `E004` | Error | Event must declare a `luma_url`. | No |

### C-family — content pages

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `C001` | Error | Content page must declare a non-empty `title`. | No |
| `C002` | Error | Content page must declare a non-empty `description`. | No |
| `C003` | Error | Blog post filename must be `YYYYMMDD_slug.md`. | No |

### D-family — matter pages

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `D001` | Error | Matter page section must be a recognized section type. | No |
| `D002` | Error | Matter page section must be in the declared kind's own catalog. | No |
| `D003` | Error | Matter page must carry the universal sections in every lens and its spine in one. | No |
| `D004` | Error | Matter page must declare a `lenses:` composition of known lenses. | No |
| `D005` | Error | Matter page section must be one its kind permits on that lens. | No |

The D-family runs over each `pages:` entry in a Project repository's `navigator.yaml`, inside the Project-manifest pass.
A matter page is never a Markdown file: a file whose frontmatter `kind:` names a page kind fails `S103`, which points at
`pages:`.

#### Declaring a matter page

A page names its type with `kind:`, its URL segment with `slug:`, and its faces with `lenses:`. The client, lawyer, and
clerk section lists live in one entry, so a page's faces cannot drift apart. Each kind has its own catalog of sections;
picking from another kind's catalog is `D002`, which is what makes per-kind checking mean anything.

```yaml
pages:
  - slug: review
    kind: review_queue_workbench
    title: Document review
    lenses:
      lawyer: [queue_rail, item_detail, item_status_setter, boundary_note, provenance_statement]
      client: [boundary_note, provenance_statement]
```

| Key | Required | Values | Checked by |
| --- | --- | --- | --- |
| `slug` | yes | lowercase letters, digits, and single hyphens; unique; not a route the matter page owns | Y021 |
| `kind` | yes | one of the matter page kinds below | Y021 |
| `title` | yes | any non-empty text | Y021 |
| `draft` | no | a repository-relative path to the page's source draft | Y021 |
| `lenses` | yes | a mapping of `client`, `lawyer`, or `clerk` to a list of section names | D001–D005 |

The kinds are `review_queue_workbench`, `verifier_split_view`, `matter_status_console`, `docket_deadline_board`,
`document_workbench`, `authority_library`, `discovery_cockpit`, `hearing_console`, `deliverable_package`,
`engagement_billing_records`, `estate_plan_console`, and `closing_room`.

Two sections are in every kind's skeleton and must appear in **every** declared lens: `boundary_note` (what this page is
not, and what still requires a human) and `provenance_statement` (as-of date, what was examined, what was not). A client
face without a boundary note is the failure `D003` exists to prevent. The kind's own spine — the sections without which
the page is not that kind — must appear in at least one lens, since a client face legitimately shows less than the
firm's. Some sections are the firm's working surface for a kind (a prebill review, a reviewer's status setter, an
assertion still under verification); `D005` keeps those on the lawyer lens.

`navigator project sync` stores the declared pages with the Project, replacing the stored set, and Navigator renders
each one at `/app/projects/{code}/{slug}` through the viewer's own lens. A manifest with no `pages:` key leaves the
stored set alone; `pages: []` clears it.

### M-family — Markdown hygiene

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `M001` | Error | Heading levels must increment by one. | No |
| `M003` | Error | Headings must use the ATX (`# Heading`) style. | No |
| `M004` | Error | Unordered list markers must be consistent. | No |
| `M005` | Error | List indentation must be consistent. | No |
| `M007` | Error | Unordered list indentation must be a multiple of two. | No |
| `M009` | Error | Lines must not end with trailing whitespace. | Yes |
| `M010` | Error | Hard tabs are not allowed. | Yes |
| `M011` | Error | Link syntax must be `[text](url)`, not the reverse. | No |
| `M012` | Error | Multiple consecutive blank lines are not allowed. | Yes |
| `M018` | Error | ATX headings must have a space after the `#`. | Yes |
| `M019` | Error | ATX headings must have a single space after the `#`. | Yes |
| `M020` | Error | Closed ATX headings must have a space before the closing `#`. | Yes |
| `M021` | Error | Closed ATX headings must have a single space before the closing `#`. | Yes |
| `M022` | Error | Headings must be surrounded by blank lines. | No |
| `M023` | Error | Headings must start at column one. | No |
| `M024` | Error | Headings must not duplicate a prior sibling. | No |
| `M025` | Error | A document must have at most one top-level (H1) heading. | No |
| `M026` | Error | Headings must not end with punctuation. | No |
| `M027` | Error | Blockquote markers must have a single space before their content. | Yes |
| `M028` | Error | Blockquotes must not contain blank lines. | No |
| `M029` | Error | Ordered list items must use the configured prefix. | No |
| `M030` | Error | List markers must have a single space before their content. | Yes |
| `M031` | Error | Fenced code blocks must be surrounded by blank lines. | No |
| `M032` | Error | Lists must be surrounded by blank lines. | No |
| `M034` | Error | Bare URLs must be wrapped in angle brackets. | No |
| `M035` | Error | Horizontal rule style must be consistent. | No |
| `M036` | Error | Emphasis must not stand in for a heading (prose only, not notation templates). | No |
| `M037` | Error | Emphasis markers must not have inner whitespace. | Yes |
| `M038` | Error | Inline code spans must not have inner whitespace. | Yes |
| `M039` | Error | Link text must not have inner whitespace. | Yes |
| `M040` | Error | Fenced code blocks must declare a language. | No |
| `M042` | Error | Links must not be empty. | No |
| `M045` | Error | Images must declare alt text. | No |
| `M046` | Error | Code block style must be consistent. | No |
| `M047` | Error | A file must end with a single trailing newline. | Yes |
| `M048` | Error | Fenced code block markers must be consistent. | No |
| `M049` | Error | Emphasis marker style must be consistent. | No |
| `M050` | Error | Strong-emphasis marker style must be consistent. | No |
| `M051` | Error | Link fragments must reference an existing heading. | No |
| `M052` | Error | Reference-style links and images must define their references. | No |
| `M053` | Error | Reference definitions must be referenced by something. | No |
| `M054` | Error | Link and image style must be consistent. | No |
| `M055` | Error | Table pipe style must be consistent. | No |
| `M056` | Error | A table's delimiter row and every body row must carry the header row's cell count. | No |
| `M057` | Error | A relative link must resolve to a file on disk, and its `#anchor` to a heading there. | No |
| `M058` | Error | Tables must be surrounded by blank lines. | No |
| `M059` | Error | Link text must be descriptive, not `here` or `click`. | No |
| `M060` | Error | Table column styles must be consistent. | No |
| `M061` | **Warning** | A published doc must not keep a relative link the renderer cannot map. | No |

`M055`, `M056`, `M058`, and `M060` read a table row the same way, through one shared reader. Cells are separated by
unescaped `|`, the outer pipes are optional and open no column, and `\|` is a literal pipe inside its cell — so a row
documenting a shell pipeline is not torn in two. Front matter and fenced code blocks are not Markdown body, so a table
drawn in either is sample text and no table rule measures it.

`M056` measures the delimiter row (`| --- | --- |`) as well as the body rows, because that row is what decides whether
the block is a table at all: GitHub-flavoured Markdown builds one only when the delimiter row's cell count equals the
header row's, and demotes the whole block to paragraph text otherwise. The demotion is silent — the pipes render
literally and the columns disappear.

### Y-family — YAML documents

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `Y001` | Error | A `seeds/*.yaml` document must be accepted by `navigator site import`. | No |
| `Y002` | Error | An English `locales/` catalog deserializes as its stem's page, or as the shared-copy contract. | No |
| `Y003` | Error | A Project repository's `blobs/**/*.yaml` pointer must name a valid asset revision. | No |
| `Y004` | Error | A Project manifest `host` must be a hostname (no scheme, port, or path). | No |
| `Y005` | Error | A Project manifest `project` must be a valid Navigator Project code. | No |
| `Y006` | Error | A Project manifest top-level key must be one of the accepted set. | No |
| `Y007` | Error | A Project manifest `no_live_row` must be a non-empty reason string. | No |
| `Y008` | Error | The Project manifest filename is `navigator.yaml`; rename `navigator.yml`. | No |
| `Y010` | Error | A Project template naming `Neon Law` with a corporate suffix must name the entity of record. | No |
| `Y011` | Error | A Project manifest must not contain YAML comments. | No |
| `Y012` | Error | A Project manifest `version` must be an exact Navigator release tag. | No |
| `Y013` | Warning | Flat `host`/`project` shape should be replaced by the versioned nested shape. | No |
| `Y014` | Error | Document pointers live under `blobs/`, never under the Git-ignored `documents/`. | Locally |
| `Y015` | Error | A manifest `skills:` entry must carry non-empty `jurisdiction`, `practice_area`, `version`. | No |
| `Y016` | Error | A nested manifest needs non-placeholder DRI emails, Slack/Notion URLs or `not provisioned`. | No |
| `Y017` | Error | Onboarding/offboarding templates cannot name Neon Law as the Firm's trade name or party. | No |
| `Y018` | Error | California onboarding/offboarding templates cannot name an LLC as the law firm. | No |
| `Y019` | Error | A `matter:` block and each pleading's `caption_title` and `footer_title` must be complete. | No |
| `Y020` | Error | A Project manifest `dockets:` must be a list of non-empty case numbers. | No |
| `Y021` | Error | A Project manifest `pages:` must be a list of entries with unique, URL-safe slugs. | No |
| `Y023` | Warning | A section in `pages:` reads a data source the live matter has no rows of. | No |
| `Y024` | Error | A Project repository tracks only Markdown, YAML, and canonical files at release bytes. | Locally |

`Y024` runs inside the Project-repository check over every file `git ls-files` tracks or would stage. A file is admitted
when its name ends in `.md`, `.yml`, or `.yaml`, and is not a package-manager file that happens to be YAML
(`pnpm-lock.yaml`, `pnpm-workspace.yaml`, `.yarnrc.yml`). The only other admitted paths are a fixed list of canonical
files: the root `.gitignore`, `.gitattributes`, `.github/CODEOWNERS`, `.claude/settings.json`, and the `LICENSE`,
`LICENSE-MIT`, and `LICENSE-APACHE-2` texts the scaffold writes. Each is held to this release's bytes. The CODEOWNERS
check and the Claude settings check already own two of them. `Y024` owns the rest. A drifted `.gitattributes` is
rewritten by local `project gate` and reported under `--ci`. The root `.gitignore` is never rewritten (LAW-181): one
that lacks `/documents/`, `*.pdf`, `*.docx`, or `*.local` is an error, one that lacks another canonical line is a
warning, and lines a Project adds are not drift. `navigator project repository sync-skills` restores the missing lines
and keeps the Project's own. A drifted licence text is always reported and never rewritten, because a licence is a grant
of rights; a Firm's own licence is `LICENSE.md`. Every other path is one error naming it. That covers a `package.json`
or lockfile, `*.ts`, `*.tsx`, `*.js`, or `*.mjs`, `*.py`, `*.sh`, code under `tests/`, and any binary, under synced
`.agents/` and `seeds/` too. `navigator project gate --tracked-files` runs only this rule, writes nothing, and reads no
network. The shared workflow's required `ci` job runs it, so a pull request that adds a script fails the check GitHub
requires.

`Y010` runs inside the Project-repository check the gate applies when the root is a Project repository. It reads each
`templates/<code>.md` or `drafts/<code>.md` and compares any `Neon Law` spelled with a corporate suffix (`, Inc.`,
`LLC`, `PLLC`, and the like) against `store::seed::FIRM_ENTITY_NAME`, the legal person a client engages. This prevents a
signature instrument from naming another party. The bare mark and `Neon Law IP LLC`, the Licensor, are not findings.

`N131` holds a template to [final documents only](notation-authoring.md#final-documents-only). Placeholders stay legal
in a template; a bracketed instruction (`[INSERT FEE]`), a fill-in blank outside a signature block, a `TODO`, `TK`, or
`XX` note, a `Note to drafter:`-style instruction line, or a blank in a certificate of service is refused, because a
final render would refuse the document it produces. A blank the signer completes by hand is a [Signer
Box](notation-authoring.md#signer-boxes): a state declared `completed_by: signer`, referenced by its placeholder, that
the render draws as an empty rule. The render's own `<!-- signer-box -->` marker typed into a template is refused the
same way as a `____`.

`Y019` reads the `matter:` block of `navigator.yaml` (court, case number, judicial officer, department, filing date,
parties, and counsel) and, for each `kind: pleading` template, its `caption_title` and `footer_title`. A refusal names
each missing field as `matter.<path>` or by its frontmatter key; `notation pdf` and `notation word` refuse the same way.
See [Pleading paper](notation-authoring.md#pleading-paper). `navigator project sync` also stores the block with the
Project as its [Matter Caption](glossary/matter-caption.md), which the `case_caption` and `party_roster` sections prefer
over the stored case row. Until the court assigns a department or judicial officer, declare `department: pending` and
`judicial_officer: pending`: `Y019` accepts it and the caption prints "To be assigned", so a draft gates and previews
without a guessed name. Replace it before filing. A manifest with no `matter:` key leaves the stored caption alone;
`matter: {}` clears it.

`Y017` applies only to `kind: onboarding` and `kind: offboarding` templates. It rejects wording that identifies Neon Law
as the Firm's trade name or contracting party. References to the Neon Law contact channel and portal remain valid.
`Y018` also applies to those template kinds, but only when `jurisdiction: CA`. California does not let an LLC render
professional services, so it rejects an LLC named as the law firm, in a greeting or as a bare signature heading. The
firm of record and the individual California lawyer both pass, and ordinary references to an LLC are untouched.

`Y014` runs in the same Project-repository check whenever `documents/` exists. The root `.gitignore` ignores the whole
folder, so a pointer left there drops out of the next `git add`. Local `project gate`, `project sync`, and `project gate
--check` move every `documents/**/*.yaml` or `.yml` file that parses as a pointer to the same slug under `blobs/`,
normalise `.yml` to `.yaml`, and remove the retired `documents/.gitignore`; a YAML file that is not a pointer stays
where it is. For this one release `--ci` reports `Y014` as a warning and leaves the checkout; the next release refuses
it. Navigator writes pointers at `.yaml`, and `POINTER_READ_EXTENSIONS` keeps the retired `.yml` spelling readable for a
pointer committed before LAW-25 — `Y003` still validates one. The retired `documents/.gitignore` is removed only once
the root `.gitignore` ignores `/documents/`; run `navigator project repository sync-skills` to add that line. Commit the
move by path: `git add blobs .gitignore` stages the Blobs and the root `.gitignore`, and `git add -u documents` stages
the removal of the old tracked pointers.

### P-family — public source snapshot

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `P001` | Error | A tracked file carries material a public source baseline may not contain. | No |
| `P002` | Error | `navigator pii` detected personal data in tracked text. | No |

`P001` names its class (`credential`, `email`, `phone`, `gcp`, `env`, `build`, `binary-metadata`, or `binary`) and the
finding description. Detected values are never printed. See [`public-snapshot-audit.md`](public-snapshot-audit.md).

`P002` runs separately through `navigator pii` in the required CI gate. It needs the pinned loopback Presidio service;
local `navigator validate` does not start that service. Diagnostics contain only file, line and category.

### F-family — files the gate had to fix

| Code | Severity | Rule | Autofix |
| --- | --- | --- | --- |
| `F001` | Error | File is not formatted; a safe-by-construction fix was withheld under `--ci`. | Locally |

`F001` fires only under `--ci`, where the gate writes nothing. Locally the same file is simply fixed and never reported,
so this code is how a formatting problem reaches a pull request instead of being rewritten in a checkout that is about
to be discarded. Run the gate and commit the result.

`Y011` runs in the Project-manifest pass. A `#` comment token is an error; the reason belongs on the pull request that
adds the entry and in the repository contract. A `#` inside a quoted or block scalar is not a comment.
