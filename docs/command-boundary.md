# The REST/OpenAPI command boundary

Every user- and tool-initiated data write — create, update, delete, and workflow actions — travels one shared command.
The four callers converge on it: the Dioxus runtime (web), the `navigator` CLI, MCP, and A2A. This keeps validation,
authorization, audit, and durable side effects defined once instead of re-implemented per adapter.

## The rule

A write has exactly one command implementation. An adapter is a thin translation over that command; it never carries its
own persistence logic.

- **Web (Dioxus).** Server functions and page/form routes render and delegate. The mutation lives in an `/app/api/*`
  command handler or a shared `portal` command module; the route is an adapter over it. Cookie-authenticated browser
  writes keep CSRF (`portal::csrf`, credential-keyed); bearer clients are CSRF-exempt but face the same actor, role, and
  embedded Rego checks.
- **CLI.** A subcommand either calls an authenticated `/app/api/*` route over HTTP (`cli/src/remote.rs`, bearer token)
  or, where it cannot depend on `portal`, calls the **same** shared `store` / `workflows` command the `/app/api` handler
  calls. `navigator project create` (`cli/src/remote.rs::projects_create`) is the HTTP form: it resolves the client and
  entity over `GET /app/api/people`/`/entities`/`/entity-types`/`/jurisdictions`, then opens the matter through `POST
  /app/api/projects` with the caller's own bearer token, so the conflict attestation stays a personal act rather than a
  shared service credential's. `store::projects::open_matter` itself is called by that route, the lawyer web form, and
  the `create_project` MCP tool — never directly from the CLI, which holds no local-store door onto it.
- **Seed reconciliation.** `navigator site import <MODEL_NAME> <SEED_FILE>` reads seed YAML locally and sends it with
  the bearer from `navigator site login` to `POST /app/api/seed`. The deployment resolves the glossary model and
  validates its `lookup_fields`, then performs lookup/create there; `--overwrite` changes only fields represented in the
  seed model. Add `--dry-run` to perform the same natural-key and foreign-reference checks without writing. It returns a
  per-record plan: each record is `new`, `unchanged`, or `changed`, and changed records name the fields that
  `--overwrite` would replace. The flags compose, so `--dry-run --overwrite` previews updates. The CLI never reads
  database credentials. With neither `MODEL_NAME` nor `SEED_FILE`, it imports every supported seed document in `seeds/`
  at the repository root instead, printing a notice and succeeding when `seeds/` is absent. GitHub Actions runs
  `navigator site import --ci --host <host>` and exchanges OIDC at `POST /auth/ci/seed-token` for a project-scoped seed
  session, without reading `~/.navigator.json`. Supported models are `person`, `entity`, `person_project_role`,
  `person_entity_role`, `address`, `relationship_log`, and `project_reference`. Join-table documents use nested natural
  keys (`person.email` with `project.code`, or `person.email` with `entity.name`) rather than UUIDs; `address` nests
  `person.email` too, keyed by `(person.email, line1, postal_code)` rather than UUIDs. A `relationship_log` record names
  its subject Person as `subject.email` and is keyed by `(subject.email, action, detail)`: the Relationship Log stays
  append-only, and re-importing an identical row appends nothing. The session's Person is recorded as its actor. An
  optional `project.code` records the Project the row belongs to; a project-scoped session always records its own
  Project and refuses a row naming another. A `project_reference` record names exactly one of `person` (`email`, `name`)
  or `entity` (`name`, `entity_type` with its `jurisdiction`) and a `reason`, keyed by `(project, party, reason)`; it
  creates the referenced row when absent and writes no participation. It is the one model through which a project-scoped
  session may create an Entity, and only as the referenced party. `navigator project reference add` sends the same
  one-record document. Add `--as-ci` to a `--dry-run` to plan under the scoped-session rules CI carries, for the Project
  this checkout's `navigator.yaml` names: the server answers a scope violation as the same `403 scope_violation` CI
  would hit on `main`, so it fails here instead. Three more models carry the estate and cap-table rows: `fiduciary_role`
  keyed by role and successor order, `estate_asset` keyed by category and description, and `cap_table_holding` keyed by
  holder (`holder_id`) and share class, each within its Project. Each takes an optional `project.code`, which a
  project-scoped session may only set to its own Project. Each names an existing Person as `person.email` or Entity as
  `entity.name` and never creates either; `--overwrite` replaces the other fields of a match.
- **Record adversity.** `navigator project adverse add (--person-email <email> | --entity-name <name>) [--subject
  entity|client_dri]` calls `POST /app/api/projects/{id}/relationships/adverse`, which lands a `manual`, full-confidence
  `adverse_to` edge from the client to a party the Project already references.
- **Document upload.** `navigator site document upload --project <code> --file <path> --kind <kind>` reads the file
  locally and sends it with the same bearer to `POST /app/api/projects/{id}/documents`. `--kind` is required and must be
  an asset-lane value — the same enum OpenAPI publishes on that operation. Run inside the matching Project checkout, it
  writes the document's Blob at `blobs/<slug>.yaml`; elsewhere it writes no local pointer. Either way it prints the
  pointer YAML. When `documents/<slug>` already has a Blob at `blobs/<slug>.yaml`, the upload files under that pointer's
  slug and refuses a `--slug` that names another (LAW-102, see [Project repositories](project-repositories.md)). From
  inside a Project checkout, `navigator project gate --check` writes the missing `blobs/<slug>.yaml` for every row this
  login can see. `navigator project sync` uses this same upload door for each staged file, then writes the pointer
  itself; it never calls object storage or writes the store directly. `navigator site document slug` uses the scoped
  document `PATCH` door to set `slug` (and optionally `kind`) on a row whose slug is null, `navigator site document
  kind` sends `kind` alone to replace a legacy kind the asset lane rejects, `navigator site document reclassify` posts
  to `POST /app/api/projects/{id}/documents/{asset_id}/reclassify` to change a chain from one accepted kind to another,
  and `navigator site document reslug` sends `slug` with `reslug: true` to move a document identity to another slug
  valid for its filename, across the row's whole chain, then moves the checkout's `blobs/` pointer and `documents/`
  bytes under `--dir` to follow it; a target equal to the current slug, or one that already names a chain, is refused.
  It checks local destinations before the live update, resolves an interrupted response from the live inventory, and
  attempts to restore the old slug if the local move fails. `navigator site document retire` posts `manual: true` to
  `.../delete` after `--yes-retire` for a mistaken chain with no pointer-removal commit; `--dry-run` reports its slug
  and revision count. It preserves the chain for 180 days and leaves local files for the caller to remove. `navigator
  site document repair` posts to `POST /app/api/projects/{id}/documents/{asset_id}/storage`, an admin door that restores
  a missing object from a same-hash sibling in the matter. `navigator site document restore` posts to `POST
  /app/api/projects/{id}/documents/{asset_id}/restore` to bring back a soft-deleted document with its revision history,
  and `navigator site document hold` posts to `.../hold` to place or release a Legal Hold. `navigator project sync`
  posts to `.../delete` only after a per-document confirmation. `project sync --only <path>`, below `documents/` or
  `blobs/`, limits uploads, downloads, pointer reconciliation, and deletion detection to that file or directory. A
  staged Authority sidecar is consumed after its pointer is written; only the pointer belongs in Git. `navigator
  validate` always checks pointer shape offline. In a Project checkout with pointers and a current stored session for
  the exact `navigator.yaml` host, it also reads each live revision chain and fails on missing or drifted pointers. An
  absent or expired session prints an explicit live-check skip; a rejected session or failed read is an error.
  Validation never uploads or rewrites a document.
- **Brand social publication.** `navigator site blog publish <file.yaml>` sends approved source and explicit providers
  to `POST /app/api/blog` using the stored site bearer. Required `account_ids` pin each approved destination. The server
  checks Brand authority, connected accounts, and MP4 dimensions, then submits the existing durable Social Publisher.
  Optional `publish_at` in the source schedules a future send. Retries preserve its workflow identity; an already-public
  Post cannot be sent again. `blog status` reads the Post and provider receipts from `GET /app/api/blog/{brand}/{slug}`.
  `blog handle` and `blog link` use the corresponding authenticated `PUT` routes for handles and existing post
  permalinks; neither sends. Every command accepts only `--host` for site selection. See
  [publish-social-post](../.agents/skills/publish-social-post/SKILL.md).
- **Public asset upload (ENG-909).** `navigator site asset upload --host <h1> [--host <h2> ...] ASSET_NAME` reads one
  local public-safe asset and sends it with the stored bearer to `POST /app/api/assets` (`portal::assets_api`), once per
  named host. Unlike document upload there is no `--project`: a public asset (a brand mark, a hero image, a font file)
  is deployment-wide. The CLI reads no GCP environment and takes no bucket name — the server resolves the bearer, checks
  the Owner/Admin tier (the same gate `PATCH /app/api/brands/{key}` uses), validates the key against `brand/`, `img/`,
  and `fonts/`, cross-checks the declared content type and `sha256` against the decoded bytes, and writes through
  `cloud::StorageService` to the deployment's own public assets bucket, never the documents bucket. This is the
  OAuth-backed sibling of the ADC-backed `navigator ops assets upload` batch command (see [`assets`](assets.md)), which
  still targets real GCS directly with an operator's own bucket credentials for bulk gallery publication.
- **MCP.** A tool in `mcp/src/tools/` translates its arguments into a shared command. The `mcp` crate cannot depend on
  `portal`, so it converges at the `store` / `workflows` layer — e.g. `link_person_project` calls
  `store::participation::add_participant` / `update_participant`, the same commands the participation `/app/api` door
  and the lawyer form use. That convergence is what makes one invariant hold at all three doors: the commands derive
  `participation` from `persons.role`, so none of the three can name one. A2A wraps the same tools behind its
  confirmation gate.
- **Open a notation.** `POST /app/api/projects/{id}/notations` and the `create_notation` tool accept exactly one code
  spelling. `template_code` is the canonical field. `draft_code` is that same code under the lawyer's name for a
  [Template](glossary/template.md) (a [Draft](glossary/draft.md)). Sending both keys is invalid even when the values
  match, and a missing, null, non-string, empty, or whitespace-only value is rejected. The server trims the chosen value
  and continues on the canonical template code. Responses, stored rows, and workflow records keep canonical field names.
  The lawyer HTML form still posts `template_code`. `project_id` (tool) and `client_email` (HTTP) stay required.
  Unsupervised MCP transports still withhold `create_notation`.

Where the command lives: `store` and `workflows` hold the persistence and durable-execution cores plus their typed error
enums; `portal` command modules hold adapter logic that must not live in `store` (matter-scope resolution, client-lens
checks). The `/app/api` handler and the web form are both thin adapters over that one command. Every `/app/api` route is
documented in `portal::api::documented_api_operations()` and `portal/src/openapi.rs`, guarded by
`server/tests/openapi_drift.rs`, and carries an authorization-matrix test across anonymous callers plus Owner, Admin,
Lawyer, Clerk, and Client. Role semantics follow [`access-model`](access-model.md): embedded Rego reads the system tier
(`persons.role`), and `participation` is derived from that same column rather than named by a caller.

## Carve-outs — paths allowed to write directly

These would be **system- and internal-initiated** writes, not user or tool commands, so they would not travel the
boundary. The allowlist in `cli/tests/command_boundary.rs` is currently empty: every write in `cli/src` and `mcp/src`
routes through the shared command boundary, with no exemption. A new carve-out needs a documented reason added both here
and to that allowlist.

Categories outside `cli/src` and `mcp/src` altogether are not part of that allowlist, but they are still
system-initiated rather than user- or tool-initiated, so they do not travel the boundary either:

- **Schema migrations and canonical catalog seed** — `store` migrations and `store::seed`. Firm-owned reference and
  bootstrap data, not a runtime write.
- **Restate durable workers** — the `workflows*` crates. A journaled handler *is* the command execution; it must never
  make an HTTP call back into `web` for its own durable side effect (see [`durable-workflows`](durable-workflows.md)).
- **Inbound webhooks** — DKIM-verified inbound email and equivalent event ingress. System-initiated by an external
  event, not a user POST.
- **Git smart-HTTP / LFS transport** — the repo-hosting path serves git's own protocol, not application commands.
- **Archive and billing batch jobs** — scheduled/library-driven writes triggered by a job, not a user action.

## Enforcement

`cli/tests/command_boundary.rs` ratchets the machine-caller adapter layers: neither `cli/src` nor `mcp/src` may
construct an entity `ActiveModel` for a write outside the (currently empty) carve-out allowlist above, and the allowlist
cannot rot (a listed file that stops writing fails the test). A new inline write in a CLI subcommand or MCP tool fails
there, pointing the author at the shared command. Web-adapter convergence is enforced per slice by each door's
authorization-matrix test plus the OpenAPI drift guard.

See also [`access-model`](access-model.md) (who may write), [`rego-policy`](rego-policy.md) (the decision engine), and
[`workspace-layout`](workspace-layout.md) (the crate map).
