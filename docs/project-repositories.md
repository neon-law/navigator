# Project workspace and repository contract

Each Navigator [Project](glossary/project.md) coordinates five distinct surfaces. They are not interchangeable stores:

| Surface | Authority | Contains |
| --- | --- | --- |
| Documents bucket | Working files | Content-addressed keys under `projects/<code>/documents/<sha256>` |
| Google Drive | Ingest dropbox | Files people drop in; Navigator copies them into the documents bucket |
| Navigator | Matter record | Project identity, participation, Notations, and asset provenance |
| Project repository | Source control | Notation templates, Project-application source, and document Pointers only |
| Matter show page | Authorized view | The matter's declared Matter Pages, rendered from `navigator.yaml` `pages:` |

Git never stores legal files or client data. Navigator-managed systems and approved file stores do. A Project's deletion
handoff contains legal files only; it does not include the repository, its source, CI output, or operational history.
Read [`public-contributor-safety.md`](public-contributor-safety.md) before preparing fixtures, planning work, or sharing
an experiment: source control and external planning surfaces carry only firm-owned or synthetic source material.

## One repository per Project, recorded as a URL

A Project has **one** repository, and the Project records **where it is** as a whole URL in `project.repository_url`. It
holds that Project's notation templates and application workspaces side by side:

```text
<the Project's repository>
├── .github/workflows/ci.yml
├── .github/workflows/cd.yml
├── .gitattributes       # keep checkout text files at LF
├── drafts/            # *.md Drafts; templates/ is the alias spelling
├── blobs/             # *.yaml Pointers: committed metadata to each document's live revision
├── documents/         # git-ignored document bytes, staged for or pulled down by project sync
├── notes/             # git-ignored call notes and memos for Communication bodies
├── seeds/             # lookup_fields / records YAML for `navigator site import`
├── .agents/skills/    # kept identical to Navigator's own canonical copies
├── .gitignore         # ignores /documents/ and notes/
├── AGENTS.md
├── LICENSE.md
└── README.md
```

**The URL is stored, never composed.** Navigator does not build `https://<host>/<org>/<code>` from a deployment-wide
forge host, because a Project's source is not the Firm's to place: it may sit on GitHub, on GitLab, on a self-hosted
remote, in an organization the Firm does not own — one Project per forge if that is how the work arrived. A composed
coordinate can express none of that, and worse, it always *exists*, so it produces a confident link for a Project that
has no repository at all.

The value is validated rather than trusted, by `store::projects::is_valid_repository_url`: `http(s)` only, a non-empty
host and path, no whitespace, and no embedded credential. That URL is handed to `git clone` and rendered to a lawyer as
a link, so a `file://` value would read the serving host's disk and a `user:token@` value would put a secret in a column
that is rendered into a page and logged.

The Project code is the stable Navigator `projects.code`, and the documents-bucket prefix is
`projects/<code>/documents`. It is also the repository's own directory name, by the convention the next section states —
that equality is why the slug rules are what they are: lowercase letters, digits, and single hyphens, alphanumeric at
both ends, at most 80 characters. A checkout and macOS are case-insensitive, so uppercase would let one directory answer
to two codes; one separator keeps the mapping an equality check rather than a normalization. The code names no Drive
folder (per-matter Drive folders are being retired; see [the glossary](glossary/project.md)), and
`project.repository_url` itself remains a stored URL Navigator never composes from the code (above).

`new` and `closed` are refused as Project codes. `/app/projects/new` is Navigator's matter-open form and
`/app/projects/closed` is the closed-matters tab, so a Project coded `new` or `closed` would collide with one of those
literal routes. Which side of a genuine collision wins depends on route registration order, so the code is refused
rather than the precedence reasoned about — in `store::projects::is_valid_code` and in an `ASSERT` on `project.code`,
because a Rust check only guards the write paths that call it.

`navigator` is refused too, for a different reason: not a route collision but a repository one. Because the repository
name *is* the code, a matter coded `navigator` in the Firm's own organization would name Navigator's own source rather
than a matter's, and every rule that treats a Project repository as client-adjacent would then be pointed at the
product. `cloud::workspace::NAVIGATOR_REPOSITORY_URL` names that one repository — one host, one organization, the same
on every deployment forever, which is exactly what a Project's repository never is — and
`cloud::workspace::RESERVED_PROJECT_CODES` carries all three refusals.

## The repository name is the Project code

A Project code is **lowercase letters, digits, and single hyphens**, alphanumeric at both ends, at most 80 characters —
no uppercase, no underscores, no other punctuation, no spaces. `store::projects::is_valid_code` is the one definition
and [the glossary](glossary/project.md) carries the rationale for each restriction. The code is the matter's whole
public identity: its show page and its repository name are all that one word — and it is **immutable**, chosen once at
matter-open and never changed. `open_matter` stores the code exactly as the lawyer supplies it — it never generates or
appends anything to it. In the normal order of operations the repository comes first: a repository's `navigator.yaml`
names its `project:` code before the matter is ever opened, so Navigator inventing a different code at matter-open would
strand that repository (and the matter's Drive folder name and Notion `Project code` URL, set the same way) on a value
it can never adopt, since the code is `READONLY` once written. A code already in use by another matter is refused as a
conflict rather than resolved by generation, so two matters can never share one code.

```text
/app/projects/<project-code>            the matter's show page — never its internal UUID
/app/projects/<project-code>/<slug>     one of the matter's declared Matter Pages
```

**A repository declares its Project in a root manifest, `navigator.yaml`, keyed `project:`.** That manifest is part of
the layout (`ALLOWED_ROOTS` in `cli/src/projects/repository.rs`), and it is the one spelling — the earlier `.yml`
extension, keyed `name:`, is retired, and a checkout still carrying it reads as an unparsable manifest. The same file
and the same reader serve both a Project repository and a staged sample-project bundle:
`store::sample_project::MANIFEST_FILE` and `cli/src/projects/repository.rs`'s `PROJECT_MANIFEST` name the identical
string on purpose, so a rename of one cannot leave the other stale. The accepted top-level keys are `host`, `project`,
`no_live_row`, `allowed_hosts`, `allowed_prefixes`, `allowed_links`, `dockets`, `matter`, `skills`, and `version`. An
unknown key is refused, naming that set. The current shape is `version: YY.M.D` plus a nested `project:` map with `host`
and `name`. The earlier flat `host:` and scalar `project:` shape remains readable with a deprecation warning. `version`
is an exact release tag, never `latest`, `main`, or `HEAD`. `project` must carry the lawyer and client DRI emails plus
private Slack and Notion URLs. Populate live values with `navigator project set --from-live`, or pass individual
`--lawyer-dri`, `--client-dri`, `--private-slack-channel`, and `--private-notion-page` flags to `project set`. The gate
rejects missing, malformed, or placeholder values. Shared Slack/Notion and Xero handles are optional. `host` is a
hostname; `name` is a Project code. An Owner or Admin with authority over the target Firm may also change live ownership
with `navigator project set <code> --firm <firm-name> --brand <brand-key>`. Supply the pair together; the server checks
the Firm wears that brand. This updates the live Project row and does not add Firm identity to the repository manifest.
`no_live_row` requires a reason. `allowed_hosts`, `allowed_links`, and `allowed_prefixes` are maps of `entry: reason`,
never lists; `allowed_links` keys are bare citation hosts linked with `rel="noreferrer"` to protect referrers. Comments
in YAML are refused because pull requests and repository contracts record reasons; quoted scalar values may contain `#`
as content.

## Document staging and pointers

Two root-level folders carry a Project's documents, alongside the chosen blueprint directory and never inside any
application. `blobs/` holds one committed [Pointer](glossary/pointer.md) per document. `documents/` holds the bytes
those pointers name, staged for upload or pulled down for reading, and the root `.gitignore` ignores the whole folder:
nothing below `documents/` is ever committed. Neither folder is a second document store.

### Matter folders

`project sync` organises `documents/` the way a case-management system organises a matter (LAW-189), and creates every
folder even when it is empty:

| Folder | Holds | `kind` that maps there |
| --- | --- | --- |
| `documents/Client Production/` | What the client supplied | `exhibit` |
| `documents/Correspondence (Served)/` | Letters served or received | `letter` |
| `documents/Discovery/` | Discovery requests and responses | none; a pointer names it with `folder:` |
| `documents/Docket Filings/` | Court paper and docket ingest | `pleading`, `filing` |
| `documents/Work Product/` | Memos, transcripts, drafts | every other kind |

The slug is the path below the matter folder: `blobs/pleadings/motion.pdf.yaml` names `documents/Docket
Filings/pleadings/motion.pdf`, and the pointer keeps the document's extension before `.yaml`. A pointer's optional
`folder:` names the [Matter Folder](glossary/matter-folder.md) when its `kind` does not, so a fresh clone downloads each
document to the same place. `onboarding/`, `offboarding/`, `invoices/`, `cases/`, and `rules/` stay siblings of the
matter folders for this release, with their bytes at `documents/<slug>`; their folder rules below are keyed to that
path. Bytes a checkout pulled down before the matter folders move into their folder on the next `project sync`, or with
the offline `navigator project repository migrate-documents`; no pointer is rewritten, so every `sha256` and asset id is
unchanged. Drop a local file into a matter folder and run:

```bash
navigator project sync
```

`project sync` derives the document slug from its path below the matter folder, uploads the bytes with that folder's
kind through Navigator's authenticated API, validates the upload receipt, and writes `blobs/<slug>.yaml` only after the
upload succeeds. The local copy stays in its matter folder. Object storage and the `assets` revision chain remain
authoritative. Bytes whose `sha256` already matches the committed pointer are skipped, and a retry of identical bytes is
a no-op at the API, so an interrupted run can safely continue. `--dry-run` names every action and question without
writing or uploading.

The pointer records `kind`, desired `visibility`, current revision metadata, and the previous asset id when the chain
has one. It never contains an object-storage coordinate or legal-document bytes. Folder conventions infer `filing` for
`pleadings/`, `exhibit` for `exhibits/`, `agreement` for `agreements/`, and `invoice` for `invoices/`; other paths use
`unclassified`. The same subfolders appear under both roots. Visibility defaults to `internal`. A folder outside those
conventions is therefore deliberately `unclassified` rather than a rejected path, and `navigator project sync --help`
says so, so a lawyer who drops a file somewhere the conventions do not name is not left reading a silent pass as a
failure. A file staged under `documents/invoices/` must match the filename pattern `INV-<digits>.<ext>` (for example
`INV-1.pdf`) or `project sync` reports it and uploads nothing.

**An executed document can record how it was signed.** The optional pointer block `signing: { method: wet_ink |
docusign, signed_on: YYYY-MM-DD }` says whether a filed original was signed in wet ink or through DocuSign, and when.
`navigator site document upload --signing-method <method> --signed-on <date>` records it on the filed revision (the two
flags travel together), and the server writes it into the returned pointer. `navigator project gate --check` and
`navigator project sync` write the live revision's record into the pointer, and keep a block recorded by hand on the
committed pointer while the live revision has none. No gate requires it, including on a `_signed` filename.

**An executed document can record its notarizations.** The optional pointer list `notarization` holds one entry per
acknowledgment in the packet. Each entry carries `method` (`in_person` or `remote_online`), `acknowledged_on`, `state`
(a two-letter code such as `NV`), `county`, and `commission_expires`, with dates as `YYYY-MM-DD`. It never holds the
notary's name or commission number, and the closed schema refuses either key. Record one entry at upload with a
`--notarization` flag whose value names all five fields, such as
`method=in_person,acknowledged_on=2026-09-04,state=NV,county=Clark,commission_expires=2029-01-31`, and repeat the flag
for each acknowledgment. `project gate --check` and `project sync` carry the list the same way they carry `signing`, and
no gate requires it.

**A served document records its service.** The optional pointer list `service` holds one entry per
[Service](glossary/service.md): `recipient`, optional `counsel_for`, `method` (`personal`, `mail`, `certified_mail`,
`overnight`, `e_service`, or `electronic_by_agreement`), `served_on` (`YYYY-MM-DD`), `address`, and `declarant`. It
never holds a Person, Entity, Letter, or email id; those stay on the live row. Record a Service with `navigator project
service add` (`--dry-run` prints the request and sends nothing), read it back with `navigator project service list` or
`navigator project service show <id>`, and `navigator project sync` writes the live records into the pointer while
keeping a list recorded by hand until the live Project has one. A pleading whose frontmatter names the served document
as `served_document:` renders its certificate of service from that list; see
[`notation-authoring.md`](notation-authoring.md#pleading-paper).

**A Communication has no pointer.** A logged call, an email, or a text is a [Communication](glossary/communication.md)
on the live Project, never a file under `documents/` or `blobs/`: `navigator project communication create` writes the
row and `navigator project communication list` reads it back, and `project sync` carries none. A call note uploaded as a
`memo` document is the workaround this replaces.

**`documents/cases/` and `documents/rules/` are not Project documents at all.** An HTML capture of a case or a statute
the matter relies on is an [Authority](glossary/authority.md) — global reference data with no `project_id` — so filing
it as an internal matter document would be the misclassification the fleet contract warns about. `project sync` routes
every `documents/cases/**` and `documents/rules/**` capture through the same door as `navigator site authorities
create`: it reads a sidecar (`<capture-filename>.authority.yaml`, beside the capture under `documents/`) carrying
`class`, `citation`, `title`, and optionally `short_cite`, `publisher`, `issued_on`, `canonical_url`, and `checked_on` —
the fields `authorities create` needs and cannot reliably scrape from arbitrary HTML `<meta>` tags — archives the bytes
as an Authority, and writes a Pointer at the capture's slug (`blobs/cases/<capture>.yaml` or
`blobs/rules/<capture>.yaml`) carrying the resulting `authority_id` alongside `sha256`, `canonical_url`, `checked_on`,
and `created_at`. The staging sidecar is removed once the pointer is filed; the ignored capture remains in `documents/`.
Only the pointer belongs in Git. A repeated exact citation with matching bytes reuses the global Authority; different
bytes under that citation require review.

The two folders split [`AuthorityClass`](../rules/src/citation.rs): `documents/cases/` holds `case_law` alone, and
`documents/rules/` holds every other class — `statute`, `regulation`, `administrative`, and `secondary`. The sidecar's
declared `class` must agree with the folder it is staged under; a mismatch (a statute staged under `cases/`, or case law
staged under `rules/`) is reported and never filed, naming the folder the capture actually belongs under. A capture with
no sidecar is reported the same way.

Every pointer `project sync` writes, whatever the route, keeps `current_version.created_at` as an RFC 3339 UTC
timestamp; `navigator project gate --check` fails a pointer that lacks one.

**The key layout stays content-addressed; a readable slug key is a proposal, not an authorized migration.** Keys remain
`projects/<code>/documents/<sha256>`, and `project sync` never renames or migrates one — the slug it derives from the
staging path names the *pointer*, not the object. Content addressing is load-bearing in three places at once: it is what
makes the `assets` revision chain immutable, what lets the same bytes filed in two matters deduplicate instead of
diverging, and what lets a governed expunge find every reference to the bytes it is about to destroy. A slug-keyed
layout has to answer all three before a migration is considered, so flattening these keys is not routine cleanup and no
part of it rides along with an unrelated change.

The root `.gitignore` carries `/documents/` as its first rule, and the command writes the canonical root `.gitignore`
when a checkout has none. A checkout whose `.gitignore` does not ignore `documents/` is refused, and `navigator project
repository sync-skills` restores the missing lines; the gate never rewrites the root `.gitignore` (LAW-181). The
repository gate refuses any tracked file below `documents/`, and any file below `blobs/` that is not a `.yaml` pointer
or the retired `.yml` spelling. Raw legal-document bytes must never be committed to a Project repository.

**Migrating from the legacy layout.** A checkout from before `blobs/` committed each pointer beside its bytes as
`documents/<slug>.yaml`, guarded by a four-line `documents/.gitignore`. A local `navigator project gate` (or `navigator
project sync`, `navigator project gate --check`, or `navigator project repository migrate-documents`) migrates it: every
`documents/**/*.yaml` or `.yml` file that parses as a pointer moves to `blobs/` (`.yml` becomes `.yaml`). The retired
`documents/.gitignore` is removed once the root `.gitignore` ignores `/documents/`; until then it stays, because the
gate never rewrites the root file, and `navigator project repository sync-skills` adds the missing lines while keeping
the Project's own. A YAML file that is not a pointer stays in `documents/`, ignored like any other bytes. Commit the
move by path: `git add blobs .gitignore`, then `git add -u documents` to stage the removal of the old tracked pointers.
For this one release, `--ci` reports the legacy layout (rule `Y014`) and each tracked legacy pointer as a warning and
changes nothing, so an upgraded Project stays green until the move is committed; the next release refuses them.
`navigator project sync --dry-run` refuses a legacy checkout until the move is made.

**`project sync` also materialises bytes into a checkout.** For every committed pointer whose local copy is absent, it
downloads the pointer's own recorded revision, verifies it against the recorded `sha256`, and writes it to
`documents/<slug>`. A local file whose digest already matches is left alone, so hydrating a fresh clone and running
`project sync` again downloads nothing. A document the caller's participation does not admit to read is reported as
`could not read` and skipped, never treated as absent, and the command refuses to write outside the checkout. The
written bytes stay where the root `.gitignore` keeps them out of Git.

Publication is all-or-nothing for the documents a run downloads. Verified bytes land in a task-owned transaction area
outside the checkout, and backups are recorded before any target is published. A publication failure rolls back every
target from those backups and leaves the before-state. If the process stops after publication starts, the next `project
sync` reads the transaction record and restores the complete before-state unless publication was marked complete, then
retries; the transaction area never creates an unknown top-level repository path. Writing a missing root `.gitignore` is
outside that guarantee.

**`navigator project gate --check` answers whether the checkout and the live record name the same documents.** It reads
the host and Project code from `navigator.yaml`. Every committed pointer must match the operative revision, and every
live document the lens can see must have a pointer. A drifted pointer is rewritten. A live document with no pointer gets
`blobs/<slug>.yaml`. The command never writes to the live site. A missing storage object needs a person: `navigator site
document repair` restores it from a same-hash sibling in the matter. A live row with no slug needs a person: `navigator
site document slug` sets the slug and, when passed, the kind. A kind the asset lane rejects needs a person: `navigator
site document kind` replaces it. `--deep` re-hashes each object, and offline hashes the Git-ignored bytes each pointer
names under `documents/` against its `sha256` and `size_bytes`: each pointer is reported `match`, `modified locally`
(fails the run), or `missing locally` (passes, since a checkout need not hold the bytes). `--ci` writes nothing: a fix
it would make fails the job and the output names the fix. Uploading a staged file, or filing a new revision, is
`navigator project sync`. Plain `navigator project gate`, without `--check`, makes no document request.

**`navigator project status` reads the live lifecycle.** It takes the Project code and host from `navigator.yaml`, or an
explicit code and `--host`. `--json` emits the full response. The caller must be a firm-side participant with the Lawyer
tier or above. The command reports the workbench label, stored `project_status`, `opened_at` (Project creation),
`closed_at`, and derived `source_state`. Onboarding evidence includes filed document visibility and generated onboarding
instruments. Filing establishes presence; execution remains unverified. A pitch names the missing onboarding work. This
command performs only authenticated reads.

**`navigator project participant list` reads the participation ledger as JSON.** Use a row's `id` with `navigator
project participant dri <id>` to designate it, or add `--clear` to remove its marker. The side follows the Person's
role; designation adds a DRI without displacing other holders. The server enforces actor authorization, records the
change in the relationship log, and refuses to clear the last lawyer DRI. Both commands take `--project` and `--host`,
defaulting to this checkout's manifest. POST and PATCH participant bodies accept `person_id` only; unknown fields,
including `is_client_dri` and `is_lawyer_dri`, fail with a named `400` error.

**`navigator project sync` reconciles the complete live Project with the checkout, in both directions.** Run it with no
positional arguments from the repository root. It reads the Project code and host from `navigator.yaml`, asks the live
site for every document visible through the caller's matter lens, and compares three things per document: the SHA-256 of
the local bytes under `documents/`, the revision the committed pointer names, and the live operative revision.

| Local bytes | Pointer vs live | Action |
| --- | --- | --- |
| match the pointer | the pointer is the live operative revision | nothing |
| absent | the pointer is the live operative revision | download |
| match the pointer | live has a newer revision | **ask**: overwrite the local copy with live revision N? |
| differ from the pointer | the pointer is the live operative revision | **ask**: upload as a new revision? |
| differ from the pointer | live has a newer revision | **conflict**: ask which side wins; nothing is merged |
| new file, no pointer | no live document under that slug | upload as a new document (asked once for the batch) |
| no pointer | a live document exists | write the pointer and download |

Bytes that already equal the live revision need only a pointer, which is also how a run that uploaded and stopped before
writing the pointer finishes on the next run, with no second upload. A pointer whose revision the live Project does not
hold is reported as `cannot reconcile` and left alone.

An upload is a new revision on the same slug, so `document log` keeps the history. Its kind and visibility come from the
pointer; the `docusign_envelope_id`, `xero_invoice_id`, `signing`, `notarization`, and `service` the pointer records are
carried onto the new pointer. The local bytes stay, and the matching SHA-256 is what makes the next run a no-op: two
runs in a row with no change write nothing to the cloud.

Every overwrite asks. The question shows the slug, both SHA-256 prefixes, the sizes, and the revision numbers, and takes
`y`, `n`, `a` (yes to the rest of the yes/no questions), or `q` (stop asking; what was answered stays). A conflict takes
`l` (local wins: upload as a new revision on top of live), `r` (live wins: overwrite the local edits), or `s`. A
declined item is asked again on the next run. `--yes` answers every yes/no question yes for scripted use and never picks
a side in a conflict. Without a terminal and without `--yes`, nothing is overwritten: the safe actions still publish,
each refused item is listed on stderr, and the command exits non-zero.

`navigator project sync --dry-run` performs the authorized inventory and prints, without writing or uploading, one line
per document: `would add` or `would download`, `would rewrite pointer` (the bytes match; only the pointer's metadata
changes), `would ask to overwrite the local copy`, `would ask to upload local edits`, `conflict` (local edits are
overwritten if live wins), `cannot reconcile`, and the batch of new files. A pointer's `docusign_envelope_id`,
`xero_invoice_id`, `signing`, `notarization`, and `service` survive while it names the same bytes; a new live revision
with different bytes drops them, and the sync names each dropped key before it rewrites the pointer.

A 401 or 403 never means "absent". A refused inventory stops the run before anything is written or uploaded. A revision
that the inventory lists but the download door refuses, or an upload the server refuses, is reported as `could not read`
or `could not write`; that item is skipped and nothing is written over it, the remaining items finish, and the command
exits non-zero. Pointer and byte downloads publish together, so a crash mid-publication rolls back on the next run.

**Deleting a pointer soft-deletes the live document for 180 days.** Remove `blobs/<slug>.yaml` from the repository, in a
commit or just in the working tree, and `project sync` treats the live document as deleted by its source: the slug had a
pointer in the repository's history, and now it does not. A live document that never had a pointer is not a deletion;
the sync writes its pointer instead. Each deletion asks yes or no on its own, showing the slug, kind, visibility,
revision count, the date it stops being restorable, and, for a client-visible document, that the client will stop seeing
it. `a` is not offered for deletions, `--yes` does not answer one, and a deletion needs a terminal or its own
`--yes-delete` flag. Declining leaves the live document untouched, and the next run asks again. A `closed_repository`
archive, and a pointer that only moved to another path (use `navigator site document reslug`), are never offered for
deletion. Bytes left behind by a removed pointer are reported, never uploaded as a new document.

A mistaken chain that never had a Pointer has no removal commit for `project sync` to act on. Retire it with `navigator
site document retire <asset_id> --project <code> --yes-retire`; `--dry-run` reports its slug and revision count. This
applies the same 180-day soft deletion and can be undone with `navigator site document restore`. Remove the local
pointer and byte copy for that slug before the next project sync.

A confirmed deletion calls `POST /app/api/projects/{id}/documents/{asset_id}/delete` with the commit that removed the
pointer. The server stamps every revision of the slug chain with `deleted_at`, `purge_after` (180 days later), the
deleting person, and that commit. Direct retirement records the deleting person without a source commit. A soft-deleted
document leaves the matter page and the default lists; a lawyer can still read it with the `include_deleted=true` option
or `navigator site document log`. `navigator site document restore <asset_id> --project <code>` clears the stamps and
returns the document with its whole revision history; the next `project sync` writes the pointer again without asking
about the deletion a second time. The retention purge, `POST /app/api/documents/purge` (admin), runs the governed
expunge with the `retention` category on every chain past `purge_after`. It skips a chain under a Legal Hold (`navigator
site document hold <asset_id> --project <code>`, `--release` to lift it) and a Project that is closed but has no
`closed_repository` archive yet, and a purged document cannot be restored. `navigator project gate --check` never
deletes.

Authority captures hydrate the same way. `project sync` also reads every committed pointer under `blobs/cases/` and
`blobs/rules/` that carries an `authority_id` and fetches its bytes from the Authority archive door, `GET
/app/api/authorities/{id}/archive` (Lawyer tier, like the write). Each download is verified against the pointer's
`sha256` and `size_bytes` before it is published, the ignored copy lands at `documents/<slug>`, and `--dry-run` lists
the copies it would fetch as `would fetch <path>`. The archived object sits at the object-storage key `blobs/<sha256>`,
unrelated to the repository's `blobs/` folder.

`navigator site document get <pointer> --out <path>` takes the Pointer path (`blobs/<name>.yaml`) or the bytes path
(`documents/<name>`) and writes outside both folders, with one exception: `--out` may name the pointer's own
sync-managed copy (`documents/<name>` for `blobs/<name>.yaml`) when Git ignores it and the fetched revision is the
pointer's current one. That refreshes a single copy; every other destination below `documents/`, and any destination
below `blobs/`, is refused, and the error names `project sync`.

`project sync` is the complete bilateral operation. It infers the kind from the folder, refuses an invoice whose
filename breaks the rule, validates each upload receipt, and files a capture under `documents/cases/` or
`documents/rules/` with its `.authority.yaml` sidecar as an Authority, keeping the bytes in `documents/`. The sidecar is
consumed after filing. `--only <path>` takes a path below `documents/` or `blobs/` and scopes uploads, downloads,
pointer reconciliation, and deletion detection to one file or directory; paths that traverse or escape through symlinks
are refused. Before downloading, `project sync` verifies that Git ignores every raw target. Only the `.yaml` pointers
under `blobs/` are eligible for source control.

`navigator validate` always checks Project pointers offline. When a current stored login matches the manifest host
exactly, it also verifies the live records through read-only requests. An absent or expired matching session produces an
explicit skip; a rejected or failed live read fails validation. It does not write pointers or bytes.

**`navigator project gate --check` answers "did it land?"** — the standalone *site document verify* command was retired
onto this one flag (26.9.24), so there is one verb rather than two for the same question. Plain `navigator project gate`
(no `--check`) checks pointer shape only — every `*.yaml` below `blobs/` must parse — and opens no connection, which is
what a pull request runs. `navigator project gate --check` additionally checks each pointer against the live asset
record using your own `navigator site login` session (the host comes from `navigator.yaml`, not a flag), reporting the
same drift `log` and `get` report: a revision missing from the live chain, a `sha256` or size that disagrees, or an
operative revision the pointer does not name; a drifted or missing pointer is rewritten in the checkout rather than
merely reported. `navigator project gate --check --ci` runs that identical live check but mints the session from the
GitHub Actions run's own OIDC token, because a runner carries no stored login, and reports every fix as a finding
instead of writing it, since the checkout is about to be discarded; that is the mode a push to `main` uses. The local
`--check` mode exists so that an operator who has just uploaded a document can confirm the asset exists remotely without
reading a CI job (LAW-12).

**Visibility stays a reviewed pointer edit.** A lawyer Project page renders a document's visibility (a plain-word
column) but offers no control that changes it. The committed pointer is replayed by `navigator project sync` through the
same authorized API that already audits the actor and operation. A button on the Project page was considered and
rejected: sharing a privileged document with a client is the exact failure this design is shaped to prevent, and a
one-click, one-confirm control removes the second reader a pull request review gives it.

**A slugless row receives its slug in place.** `navigator site document slug` sets `slug`, and optionally `kind`, on a
row whose slug is null. The server refuses a row that already has a slug, and a slug that already names a revision
chain, so the write never merges two documents. `sha256`, the storage key, and the bytes stay unchanged. Moving a slug
that already exists is `navigator site document reslug`, not this command. A missing storage object is `navigator site
document repair`, which copies bytes from a same-hash sibling in the matter onto the content-addressed key, or re-points
the row at that key, then verifies the sha256. It does not expunge the document.

**A re-upload creates a Document revision.** A pointer has no slug field; its path below `blobs/` is the slug. When
`navigator site document upload --file documents/drafts/n.md` finds `blobs/drafts/n.md.yaml`, the upload files a new
revision of `drafts/n.md` and prints that slug, wherever the command was typed: the checkout is the nearest directory
above the file whose `navigator.yaml` names the Project. A `--slug` that differs from the pointer's, a pointer that
cannot be parsed, a retired `.yml` pointer, and `.yaml` and `.yml` side by side are each refused before anything is
filed, because filing anyway would start a second chain that no `site document` verb can retire. To start a separate
document, upload a copy that has no Pointer. A file with no Pointer keeps the path-below-`documents/` slug, and a file
outside `documents/` is never matched to a pointer. The checkout is the nearest directory above `--file` whose
`navigator.yaml` names the Project, whether or not a Pointer exists, so a first upload typed from another folder writes
its pointer to `blobs/` in the file's own checkout rather than the working directory. A pointer written before slugs
carried their extension (a chain filed as `summons`, pointed at by `blobs/summons.pdf.yaml`) is named by its path, as
`project sync` and the gate already name it; `reslug` makes the chain's slug match.

**Upload a directory as a batch.** `navigator site document upload --project acme --dir exhibits --kind exhibit` files
regular files in filename order without recursing or following file symlinks. Choose exactly one of `--dir` and
`--file`; `--slug` applies only to `--file`. Each file uses the same Pointer resolver, folder, visibility, and extension
rules as a single upload. SHA-256 matches within a batch are skipped only after the first copy is successfully filed.
The summary reports every file, including duplicates and failures. A failed or uncertain upload also blocks later copies
of its bytes from being sent again. Successful uploads and their pointers remain when another file fails, and any
unfiled file makes the command fail.

An expired session pauses the batch once for the supported browser sign-in, reloads the stored credentials, and resumes
with the remaining files. Only a definite authentication refusal permits retrying the current upload. A transport error
or an unreadable success receipt requires reconciliation with `navigator project gate --check` before retrying; the CLI
never silently repeats that write. `--description` applies the same optional description to every file.

**A document identity moves with its chain.** `navigator site document reslug <asset_id> --project <code> --slug <slug>`
moves a document to another slug valid for its filename, such as `notes/a.pdf` to `filed/forms/a.pdf`. Every revision
moves together, in the same order, with `sha256`, the storage key, byte size, and kind unchanged. The server refuses a
target equal to the current slug and a target that already names a chain, so two documents never merge. `--dry-run`
reports the move and writes neither the row nor the checkout. After a real move the command renames the checkout's
`blobs/<old>.yaml` pointer and, when present, the bytes at `documents/<old>` to `blobs/<new>.yaml` and `documents/<new>`
(`--dir`, default `.`). It checks both destination paths before changing the live identity. If the live response is
interrupted, it checks the current inventory and completes the local move when the live slug changed. If the local move
fails after the live change, it attempts to restore the old live slug. `navigator project gate --check` reconciles a
pointer when that recovery cannot complete.

**A legacy kind is replaced in place.** A row can carry a free-text kind from before the closed asset-lane list, and the
gate rejects its pointer. Pass `--kind` to `navigator site document slug` while the row is slugless; once it has a slug,
a revision cannot change its kind, so `navigator site document kind` replaces it on the row and every revision of its
chain. The server refuses a row whose kind is already accepted, so it never turns one valid classification into another.
`sha256`, the storage key, and the slug stay unchanged.

**A wrong accepted kind is corrected in place.** A document filed under an accepted kind that is still the wrong one — a
lawyer-review draft filed as `memo` that belongs under `unclassified` — is corrected with `navigator site document
reclassify <asset_id> --project <code> --kind <kind>`. It rewrites the kind on the row and every revision of its slug
chain, and leaves `sha256`, the storage key, byte size, and the slug unchanged. The server refuses a target equal to the
current kind, and a row whose kind is not accepted, which is `navigator site document kind`'s case. `--dry-run` reports
the write without making it. Lawyer or Admin only; the API audit records the actor and operation. After it succeeds,
`navigator project gate --check` rewrites the pointer's `kind`.

**An EFSP status email records an E-Filing Transaction.** `navigator project efiling record <email.eml>` reads a status
email from an e-filing service provider, takes the transaction id from it, and records the reported status (`submitted`,
`accepted`, `rejected`, or `unknown`) on the Project. An email that names the transaction but not its status records
`unknown`, with a note to log in to the provider, never `accepted`. When an acceptance email attaches the court-stamped
PDF, the command files it as kind `filing` under `efilings/<transaction id>.pdf`, then records its asset id, the
court-filed date, and the acceptance date on the transaction. The filed date and the acceptance date are kept apart, and
both are refused on a transaction that is not accepted. Filing a stamped copy needs the submitted revision: pass
`--submitted <asset_id>` the first time, or the command refuses before uploading. `navigator project efiling list`
prints the recorded transactions. See [E-Filing Transaction](glossary/e-filing-transaction.md).

**A hand-downloaded stamped copy is paired, not uploaded.** When the provider's email carries no PDF, the attorney
downloads the stamped copy in their own signed-in browser and runs `navigator project efiling pair <pdf> --transaction
<id> --filed-on YYYY-MM-DD`. It files the PDF under the same `efilings/<transaction id>.pdf` slug, records the
transaction as accepted with that filed date, and links the copy to the submitted revision, which `--submitted` names
when the transaction does not already. A transaction recorded as rejected is refused. `--judge-signed` marks a proposed
order that came back only as a received copy, so the transaction shows that the entered order is still needed.

**The Portal is retired.** `/app/projects/<code>/portal`, `/app/projects/<code>/portal/`, and every path beneath them
answer `301 Moved Permanently` to the matter show page `/app/projects/<code>` (`portal/src/project_portal.rs`). No
bundle is read or streamed, and the web process holds no applications-bucket handle. A matter's client-facing pages are
its Matter Pages, declared under `pages:` in `navigator.yaml` and rendered on the show page. Each sample repository is
still named for the code it mounts on, so `neon-law/sample-litigation` is the repository of `sample-litigation`; that is
a naming discipline, not an enforced rule, and the manifest settles which code wins.

The applications bucket itself is still provisioned. Objects under a retired `<code>/portal/` prefix stay there until an
operator deletes them by hand; nothing reads them.

During local development, `navigator dev up` and `navigator dev worktree-env up` clone and stage each sample project's
source (`navigator.yaml`, `drafts/`, and the commit) before writing `.devx/env`. They run no `pnpm` and stage no
`dist/`.

## Court notices

A litigation Project lists the case numbers it appears in under `dockets:` in `navigator.yaml` (`Y020` refuses anything
but a list of non-empty strings). `matter.case_number` counts too. Save each CM/ECF [Notice of Electronic
Filing](glossary/notice-of-electronic-filing.md) as an `.eml` and run, from the repository root:

```bash
navigator project docket ingest notices/*.eml --dry-run
navigator project docket ingest notices/*.eml
```

The verb reads the Project code and host from `navigator.yaml`; there is no `--project`. A notice for any other case is
skipped as "not a docket of this Project", without naming the case. `--only <doc_no>` files one entry, and
`--skip-pattern <text>` drops notices whose docket text contains the text (for example `transfer of claim`). Both
repeat. `--dry-run` reads the notices and the checkout and prints the plan; it downloads nothing and contacts no server.

Each document lands at `documents/Docket Filings/DKT NNNN - <title>.pdf`, with its Pointer at `blobs/DKT NNNN -
<title>.pdf.yaml`. The suggested kind is `pleading`, `exhibit` for an exhibit attachment, or `transcript` for a
transcript; `--kind` overrides it. A free-look link is single-use, so bytes are written to disk before anything else. A
failed upload leaves them there, and the next run uses them without spending another free look. An entry already filed
is skipped by its DKT identity or an existing Pointer matching the notice's original filename. Distinct docket numbers
keep distinct Document Identities even when their bytes match. Each numbered attachment gets its own linked
Docket Entry.
`docket add --slug` and `docket link-document` apply the linked entry's DKT name too. `docket renumber --apply`
reconciles existing names; ambiguous links and colliding names stop it.

## Existing Project setup

One CLI command converges an existing Project's Drive/repository surfaces and its Firm-private Slack channel and Notion
page through the existing authenticated doors (`portal::integrations_api`):

```bash
navigator project setup <code>        # one Project, one outcome per resource
navigator project setup --all --json  # every Project visible to this login
```

Setup composes the surface reconcile door, Slack ensure door, and Notion ensure door. It does not accept provider
credentials or open a database connection. The doors keep their existing admin, visible-Project, and owning-Firm
`FirmCapability::UseIntegrations` checks. Text prints one row per resource; `--json` returns one structured resource
outcome per Project. Any required resource outcome other than `created`, `present`, `adopted`, `unchanged`, `repaired`,
or `notified` makes the process exit nonzero, including `credential_missing`, `provider_unavailable`,
`recorded_resource_missing`, `wrong_parent`, `not_private`, `renamed`, `archived`, `conflict`, and
`address_not_recorded`. Setup reports no `publisher` resource; Portal publishing is retired.

An unconfigured Drive or repository provider reports `skipped` with a detail naming the disabled provider; that outcome
exits nonzero. A partially configured provider fails the reconcile door with its initialization error.

Setup never replaces a recorded provider identity. A deleted or otherwise unresolvable recorded resource is reported as
`recorded_resource_missing`, and a provider privacy or Notion-parent mismatch is reported for operator remediation. The
Slack id and canonical URL are persisted in one Project-row update. If one door fails, the other doors still run; rerun
setup after the provider or credential is repaired. The in-process lock prevents duplicate work within one Navigator
replica. Across replicas, the guarantee is only the provider adapters' find/create idempotency and the single-row
recorded-identity check; this command does not claim a distributed exactly-once lock.

**Prerequisite: a Firm credential.** Every door resolves the caller's `FirmCapability::UseIntegrations` against the
target Project's owning Firm and then reads that Firm's typed provider credential (`store::firm_secrets`) — there is no
deployment-wide fallback. Until the Firm's Admin DRI has stored a `slack_bot_token` / `notion_token`
(`/app/admin/firms/{id}` → **Integration secrets**, ENG-491), every one of these commands reports `credential_missing`
for that Firm's Projects, and a deployment with no runtime KMS key at all reports `runtime_not_configured`.

**Exit codes reflect the typed outcome, not just the HTTP status.** Every door answers HTTP 200 with a per-Project
`{"project_code", "outcome", "detail"?}` row (`credential_missing`, `provider_unavailable`, `conflict`, `renamed`,
`archived`, `missing`, `duplicate`, `address_not_recorded`, … are all outcomes, not failures at the transport level),
and the CLI is the layer that turns that into a process exit code. `created`, `adopted`, `unchanged`, `repaired`, and
`notified` are the only outcomes that exit `0`; every other named outcome, and any outcome slug this CLI does not
recognize, exits non-zero — including one failing row inside an `--all` sweep whose other rows succeeded. `--json`
prints the same report a text-mode run summarizes one line per Project; both modes agree on the exit code.

**Identity is validated, never silently replaced.** `ensure` and `reconcile` look up an already-recorded resource by its
own id (not by a fresh title/name search) before touching it, because a search cannot see a page or channel that was
renamed away from its recorded title or archived. A recorded resource that has been renamed, archived, or otherwise
drifted is reported (`renamed`, `archived`) rather than replaced or duplicated; only a Project with no recorded resource
at all is eligible for create-or-adopt. Concurrent `ensure` calls for the same Project and provider serialize on an
in-process lock keyed by `workflows::IntegrationJob::job_id`, so a race cannot create two resources for one Project — a
single-process guarantee, backed underneath by the provider adapters' own idempotent find-then-create.

## The organization is configuration, not a name in source

Navigator spells no organization in its source, and one forge host: the named default
`cloud::workspace::DEFAULT_GIT_HOST`. `NAVIGATOR_GITHUB_ORG` names the organization this deployment's *own* automation
occupies, and `NAVIGATOR_GIT_HOST` names the host it lives on. The two are one `(host, organization)` coordinate — where
this deployment's Project repositories are created, and the boundary `ops github setup` refuses a governance write
outside. Two organizations are admissible on a governance write: the public one holding Navigator itself, and this
deployment's own. A remote in neither is refused before a token is read.

Repository provisioning uses the deployment's GitHub App when the App ID, installation ID, and private key are all set.
Navigator signs a short-lived App JWT, exchanges it for an installation token, and reuses that token until it nears
expiry. A partial App configuration is an error; it does not fall back to a broader personal token. When no App
credentials are configured, `NAVIGATOR_GITHUB_TOKEN` takes precedence over `GITHUB_TOKEN` for standalone setups.

Only the host carries a default, and only because it has a right answer. An organization has none, so a named deployment
must state it. `cli/tests/forge_coordinate_retired.rs` is the guard that keeps the organization out of source, admits
exactly one spelling of the host — that constant's own declaration — and keeps neither from being composed into a
Project's URL.

**Neither names a client matter's source.** That is `project.repository_url`, which is data. A deployment's organization
and a Project's repository are independent: a deployment can serve a matter whose source lives in a client's own GitLab
group.

| Deployment | GCP project | Organization | Drive root |
| --- | --- | --- | --- |
| Staging | `neon-law-stg` | `neon-law` | `Staging Projects` |

The active deployment is identified by `NAVIGATOR_GCP_PROJECT_ID`. It is deliberately not `NAVIGATOR_ENVIRONMENT`, which
is a two-valued dev/production switch and cannot name a deployment.

### An absent repository is legitimate

A Project may record no repository at all, and that is correct rather than degraded: a matter opens before anyone
creates its source, so `project.repository_url` is nullable and the lawyer matter page simply omits the pointer.

Nothing invents one to fill the gap. That is the whole difference from the derivation this replaced — a composed
coordinate always existed, so it rendered a confident link whether or not the repository did, and when the forge host
fell back to a public default it aimed that link at a namespace the Firm does not control. `ops github setup` documented
having no public fallback while the pointer that actually served users had one.

| State | Answer |
| --- | --- |
| `repository_url` recorded | The lawyer sees it verbatim. Never verified — the target may not exist yet. |
| `repository_url` absent | The pointer is absent. Not an error, and nothing is composed. |
| A value that is not an `http(s)` URL with a host and path | Refused at the write, not stored. |

### Reconciling the rows against what they record

`GET /app/api/project-repositories` reports where a Project row and its `repository_url` disagree, across every matter
in the deployment. It reads rows rather than checkouts, and both halves of that are deliberate.

A scan of local clones would make every answer conditional on the operator having cloned the whole fleet, and silently
wrong when they have not — in a Project repository's own CI run exactly one checkout is present, so every other matter
would read as a repository that is gone. And the matter list is a lens rather than an inventory:
`store::access::visible_projects` scopes to the caller's participation rows for **every** firm tier, Owner and Admin
included, so a read through it cannot tell "no such row" from "not yours".

So the door is admin-tier and reads all rows directly. That tier is the control rather than a precaution: reading every
matter is the privileged reach the matter surface refuses to grant silently, so it is a door an administrator navigates
to. It discloses one code and one repository URL per matter, and no matter content.

| Finding | Severity | Needs the forge pair |
| --- | --- | --- |
| `repository-name-is-not-code` — the recorded URL names a different repository | fail | no |
| `records-navigator-itself` — the row records Navigator's own repository | fail | no |
| `duplicate-repository-url` — two matters record one repository | fail | no |
| `repository-url-invalid` — a value today's write gate would refuse | fail | no |
| `no-repository-url` — the row records none | warn | no |
| `repository-outside-deployment-forge` — recorded somewhere this deployment would not have created it | warn | yes |

Every **failing** finding is computable from one row and a rule, because the code is the repository name. Only the last
needs configuration, and it is a warning by design: a Project's source may live on any forge, in an organization the
Firm does not own, which is the state a stored URL exists to permit. Where a deployment has no pair configured, the
report carries `compared_against_deployment_forge: false`, so an absent warning is never read as agreement.

Findings serialize internally tagged — each carries its own fields beside its `kind` and `severity` — so a consumer
reads what it needs by name rather than parsing a sentence.

### Provisioning the three handles

Opening a Project records its identity, then `store::project_surfaces` creates or adopts its three external surfaces:
the documents-bucket prefix `projects/<code>/documents` (a key convention; nothing is written), the Drive ingest folder,
and a private repository named for the code. Setup reports GitHub separately. Provisioning is idempotent and adopts
existing folders and repositories. A recorded `repository_url` stays unchanged, so Projects hosted on another forge are
not moved. When every configuration key for Drive or the forge is absent, setup reports that surface as `skipped` with a
reason and exits nonzero; partial or invalid configuration returns an initialization error. Matter-open still runs
provisioning best-effort so a provider fault does not roll back an open Project.

### Importing from the Drive dropbox

Drive is an optional ingest dropbox; object storage stays the working-file authority. A lawyer on the matter imports the
dropbox with `POST /app/projects/{code}/documents/drive-import` (`visibility`, optional `kind` and `description`).
`store::drive_import` verifies the recorded folder by id inside the deployment's Projects drive and refuses a folder
another Project records, so the folder's display name need not equal the Project code. Each download is checked against
Drive's listed size and MD5 and screened by the attachment scanner before it is filed; a mismatch or a finding is
refused and reported, and nothing is written for it. Each Drive file is one document identity (`drive-<file id>`): an
unchanged rerun writes nothing, a new Drive version becomes a new revision carrying its Drive version, modified time,
and checksum, and an interrupted pass converges when run again. A pass admits at most 50 files, 25 MiB each, 500 MiB in
total. Every later read comes from object storage. A deployment without Drive answers `409`.

**A Project repository is created private, and provisioning verifies rather than assumes it.** `GitHubForge` requests
`"private": true` explicitly on every create — never the organization default, which member permissions or an enterprise
policy can loosen without this deployment noticing — and re-reads the repository afterward, on both the freshly created
path and the adopt-on-name-conflict path, failing closed if it is not private. That failure names a policy regression or
a deliberate visibility change, either of which needs a human rather than a silent retry.

`POST /app/api/project-surfaces/{id}` is the admin retry for a failed or legacy row. It carries its own noun rather than
sitting under `/app/api/projects/`, because that prefix's GET rule admits any authenticated caller up to five segments.
CLI: `navigator project setup <code>` — an HTTP client of this same door, like every other `navigator site` command. It
resolves the given code to an id through `GET /app/api/projects` (the same lookup `navigator project close` uses) and
never opens a database connection of its own, even against a local deployment. Project participation is never copied
onto the forge.

## The branch contract

Every Project repository follows one branch contract:

- `main` is the only long-lived branch.
- Every change reaches `main` through a pull request. Nobody pushes to it directly.
- Pull requests are squash-merged. Merge commits and rebase merges are off.
- The head branch is deleted when its pull request merges.
- The required check is `ci`, which runs `navigator project gate` (see [The CI gate](#the-ci-gate)).
- Open pull requests ready for review by default; use a draft only when deliberately holding work for more changes.
  The generated GitHub workflow verifies `autoMergeRequest` after arming, unless the PR has already merged. If it is
  still open and unarmed, run `gh pr merge <number> --auto --squash`; confirm `autoMergeRequest` is non-null. Missing
  App credentials require this manual fallback. On another forge, use its native ready-for-review and
  merge-when-checks-pass controls and verify them; GitHub's `autoMergeRequest` field does not apply.
- Before merging, review every changed document pointer (`blobs/**/*.yaml`): retain `visibility: internal` unless
  the client is meant to read that document. Treat every change to `visibility: client` as a deliberate client-release
  decision. For each Deadline, choose deliberately whether it is internal or client-visible and review that choice.
- Publishing a page or a Draft means merging its pull request.

The forge enforces the branch and merge settings; client-visibility decisions remain part of human review. Project
repositories are moving to a Forgejo forge with a GitHub account connected to it, and the forge's branch protection and
merge settings carry those controls. `navigator ops github setup` is deprecated. Until the move completes, it keeps the
existing GitHub repositories on this contract. A local pre-commit hook that runs `navigator project gate` is a
convenience only: a contributor can skip it, so it never stands in for the required check.

The canonical `AGENTS.md` states the same contract, including deliberate client-visibility review and PR readiness. A
change to it reaches each Project repository when that repository runs `navigator project repository sync-skills` at its
next CLI bump.

## The CI gate

A Project repository's `ci.yml` and `cd.yml` are thin callers of Navigator's reusable workflows, pinned to one exact
release tag. `navigator.yaml` carries the same CLI version; the gate reads it after checkout and before downloading the
the CLI. `ops github setup` reconciles both callers and the manifest pin, and does not resolve `latest`. Validate does
not check that the pin exists on the registry. A tag that was never published fails on the run that downloads it.

```yaml
jobs:
  ci:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@YY.M.D
```

No `with:` block: the reusable workflow reads `project`/`host` from the caller's own checked-out `navigator.yaml`, so
the pinned tag in `uses:` is the only value a caller repeats. `ops github setup` refuses a caller that declares one.

The caller is pull-request-only and has no permissions or inherited secrets. A called workflow cannot widen the token
the caller grants it, so fork PRs cannot mint OIDC credentials; the reusable gate's live jobs remain main-only, and
`cd.yml`'s own `gate` job (below) is what re-invokes this same reusable workflow on a push to `main` so those live jobs
actually run. The `ci` job is the required check, but because it *calls* a reusable workflow rather than running its own
steps, GitHub posts no check run under its own name at all — it posts one per job inside the *called* workflow, named
`<caller job id> / <called job id>`. `project-gate.yml`'s own terminal job is spelled `ci` too, so the required status
check `ops github setup` binds a Project repository's ruleset to is the compound context `ci / ci`, not the bare `ci` a
repository whose `ci.yml` runs its own steps (like Navigator's own) requires.

The reusable gate workflow declares three feeder jobs — verify, documents, and seeds — for the Project check. A
malformed manifest is reported against `navigator.yaml` and stops the template pass, so one bad map cannot produce
misleading findings. Each feeder job runs unconditionally and no-ops over a half this repository does not carry.
`verify` installs the CLI through `.github/actions/navigator-install` and runs `navigator project gate --ci` over the
whole tree: content rules and layout. It builds nothing; a Project repository carries no application to build. On a push
to `main` the same run also checks `navigator.yaml` against the live row.

The `documents` job runs `navigator project gate --check --ci`. `--check` (LAW-62) runs only the live document check —
comparing committed pointers with the live record — and none of `verify`'s offline passes. The host and Project code
come from `navigator.yaml`, and the session is minted at `POST /auth/ci/document-token`. It runs unconditionally
alongside the others. A repository with no `blobs/` directory still runs the check, so a live document with no pointer
is reported. `--ci` writes nothing: a fix the gate would make locally fails the job. It is one of the required check's
dependencies.

**This mint is not a GCP credential.** `POST /auth/ci/document-token` (and `seeds`' own `POST /auth/ci/seed-token`)
verifies GitHub's OIDC token directly against GitHub's own issuer and returns an HMAC-signed Navigator session — a
project-scoped API credential, nothing more. Neither job authenticates to GCP, federates an identity into one, or holds
a GCP credential of any kind; `id-token: write` here grants only the token these two doors read. No job a Project
repository calls reaches GCP.

The `seeds` job retains its main-only live check. `navigator project gate` covers the offline shape of every
`seeds/*.yaml` document on pull requests, while the live reconciliation remains outside the required `ci` dependencies
because it needs a reachable deployment.

**There is no path filter, and that is deliberate.** A filtered job that skips reports success for work it never did,
and a required check a skip can satisfy is not a gate. So every job always runs and each half no-ops over a repository
that does not carry it, rather than being skipped. The required job is spelled `ci`, which is the one context `navigator
ops github setup` binds — and, because a **skipped** job reports no result at all, that job asserts each feeder job's
`result` explicitly rather than trusting a bare `needs:`, which a skip would satisfy silently.

What the gate proves:

- The layout is source-only. Client uploads, answers, generated documents, secrets, dependencies, and build output are
  refused by path and by extension.
- Every direct `templates/<code>.md` or `drafts/<code>.md` passes the notation rules. Each template's `code` equals its
  filename stem. No template names `Neon Law` with a corporate suffix other than the firm entity of record (`Y010` in
  [`docs/gate.md`](gate.md)). Onboarding and offboarding templates also name the Firm by its legal entity, not as
  trading under Neon Law (`Y017`). California onboarding and offboarding templates cannot name an LLC as the law firm
  (`Y018`); a template names the firm of record or the individual California lawyer.
- A Vite application under `apps/<app>/` or a root `portal/`, and any other `package.json`, lockfile, or repo-local
  script, is a warning that points at `pages:`: a Project repository runs no tooling of its own, and the gate no longer
  validates an application's shape or scripts.

Pin both callers to an exact immutable release tag (`YY.M.D`, `YY.M.D-rc.N`, or `YY.M.D-hotfix.N`), never `main` or
`latest`. Publishing a rolling pointer is allowed; consuming one is not. The tag must also be one this repository
actually published: a `uses:` at a ref that does not exist fails the run outright with "unable to resolve action", and
unlike a renamed repository — which GitHub redirects, so the old spelling keeps working — a missing ref has nothing to
redirect to. The shape rule is machine-checkable and `validate` enforces it; whether the tag exists is not, which is why
`ops github setup`'s `--action-version` derives the pin from a confirmed release rather than accepting a version someone
typed from memory.

## The retained publish caller

`cd.yml` is the thin main-only caller of two reusable workflows, both pinned to the same `YY.M.D`. Its `gate` job
re-invokes `project-gate.yml`, the same file `ci.yml` calls on pull requests, so the live Project gate and the seed
import that only run on a push to `main` actually run on one. The documents job's live check runs on pull requests too.
Its `publish` job `needs: gate` and calls `project-publish.yml`.

**`project-publish.yml` is a retained no-op.** Portal publishing is retired, so the workflow builds nothing, reads no
publisher variable, and uploads nothing; its one job only prints that there is nothing to publish. It stays callable so
every existing `cd.yml` keeps passing, and its deprecated `project`/`host` inputs are accepted and ignored. The
`.github/actions/application-publish` action, the per-Project publisher service accounts, and the
`NAVIGATOR_APP_PUBLISHER_*` repository variables are gone with it.

The thin caller workflow lives in the Project repository. `ops github setup` writes and reconciles
`.github/workflows/cd.yml` for a confirmed Project repository, so it never hand-copies a job implementation:

```yaml
# <project-code>/.github/workflows/cd.yml — an example of what a
# Project repository contains, not a file in this repository.
name: cd
on:
  push:
    branches: [main]
  workflow_dispatch:
permissions:
  contents: read
  id-token: write            # both main-only callers below mint WIF
jobs:
  gate:
    uses: neon-law/navigator/.github/workflows/project-gate.yml@YY.M.D
  publish:
    needs: gate
    uses: neon-law/navigator/.github/workflows/project-publish.yml@YY.M.D
```

Neither job carries a `with:` block, for the same reason `ci.yml` above does not: both reusable workflows read
`project`/`host` from the caller's own `navigator.yaml`.

Seed and document reconciliation remains in the reusable gate's main-only jobs, and `gate` (above) is what runs them:
`ci.yml` calls the same reusable workflow only on `pull_request`, so nothing else would trigger their live half. The
caller carries no deployment variable or CLI command; its deployment coordinates come from `navigator.yaml`.

### The sample Project lane

Three public, read-only snapshots — `neon-law/sample-litigation`, `/sample-transactional` and `/sample-estate` — are
headless: each declares its matter's pages under `pages:` in `navigator.yaml` and keeps its Drafts under `drafts/`, with
no Portal, no Node, and no lockfile. `navigator dev sample-project` clones each into a temporary directory and stages
the manifest, the Drafts, and the commit under `.devx/sample-projects/<code>/`. The next local boot stores the staged
`pages:`, persists each Draft pinned to that commit, opens the matter's Notations, and writes the records each declared
section reads (`store::seed`'s sample records), so every page renders from the store.

The example at `docs/examples/sample-portal-publish.yml` is the same thin `cd.yml` caller `ops github setup` reconciles
a Project repository's caller to. It passes only the release pin; it contains no build script, Python, bucket name, or
credential.

The CLI parser refuses unknown manifest keys by naming the accepted set.

The shared reusable-workflow home is this Navigator repository today. A Project repository consumes one `uses:` line for
CI and one for CD; it does not copy the job implementations or any Python helper. `ops github setup` accepts a
`--workflow-home <owner>/<repo>` override (default: the current source slug) that repoints both `uses:` lines and the
required-check binding. The sample snapshots on GitHub are public with Actions disabled: they are a seed source for
local development, and their workflow files are shown, not run. GitHub does not let a public repository call a private
reusable workflow. Staging's sample repositories live on that deployment's Forgejo and are gated there.

## A repository's fixed shell

Nothing generates a Project repository's shell; it is built by hand — a checkout, `git init`, and the files below,
copied from Navigator's own repository root or another Project repository — and then checked by `navigator project
gate`:

```bash
navigator project gate
```

The shell a fresh repository carries: `.gitattributes` pinning checkout text files to LF, `.github/CODEOWNERS` with the
canonical `* @shicholas` ownership rule, a versioned nested `navigator.yaml`, the thin PR-only `ci.yml` caller, the thin
`cd.yml` caller for pushes to `main`, `README.md`, `AGENTS.md`, and the canonical `.agents/skills/` catalog copied
byte-for-byte. It also carries a root `.gitignore` that ignores `/documents/` and `notes/`, and one `drafts/<code>.md`
for the notation the Project actually opens on. A repository commits only Markdown and YAML (rule `Y024`): every tracked
or stageable file is `*.md`, `*.yml`, or `*.yaml`, apart from the canonical `.gitignore`, `.gitattributes`,
`.github/CODEOWNERS`, and `.claude/settings.json`, each held to this release's bytes. No `package.json` or lockfile, no
TypeScript, JavaScript, Python, or shell, no `tests/` code, and no binaries; the finding names each path. A `tests/`
folder may hold Markdown notes, but anything worth checking about a template or seed is a gate rule in the CLI instead.
`AGENTS.md` is the only portable contract file and `.agents/skills/` the only skill catalog. The narrow `.claude/`
exception is root `.claude/settings.json`. It denies Claude Code Gmail send, reply and forward tools while preserving
draft creation. The rules match the tool name under any MCP server prefix — `claude_ai_Gmail` in the CLI, a connector
UUID in the desktop app — plus the Codex Gmail tool families, and hold in bypass-permissions mode. The gate requires it
byte-for-byte. The gate names retired mirrors and gives a remedy instead of anonymous root-layout wording. It also
refuses an `apps/<app>/CLAUDE.md`. `navigator project gate` requires the canonical CODEOWNERS file and that `AGENTS.md`
exist and name the Lawyers team as where a Navigator CLI gap is filed rather than recorded as a workaround in the matter
repository. The same `validate` walk extracts every *navigator …* invocation from the repository's Markdown and checks
each against this binary's clap command tree, so a documented verb that does not exist fails the gate at the commit that
introduced the rename.

**`AGENTS.md` is one canonical file everywhere.** It carries the same short tool list and Navigator safety contract in
every Project repository. Project-specific identity and coordinates belong in `navigator.yaml` and the live Project row,
not in checked-in instructions. The gate compares the file byte-for-byte with the copy compiled into the CLI, just as it
does each synced skill. Restore missing or drifted canonical skills and `AGENTS.md` with `navigator project repository
sync-skills`; it writes only those canonical paths and preserves custom skills. Run `navigator project gate` afterward
to verify the repository.

For a Project version-bump PR, use the CLI from the exact target release. `ops github setup` then includes missing or
drifted canonical Agent Skills with the workflow and manifest pins. Legal Council's cited examples are embedded in that
binary and copied into `.agents/skills/legal-council/SKILL.md`; custom skills remain untouched. A different
`--action-version` updates pins but prints a diagnostic requiring the matching release CLI for skills. Follow the fleet
sweep in the [`cut-release` skill](../.agents/skills/cut-release/SKILL.md#fleet-sweep).

Drift in a synced skill, `AGENTS.md`, or `.claude/settings.json` is repaired with `navigator project repository
sync-skills`; the gate detects the mismatch. These canonical files are embedded in that CLI release. A repository
carrying a retired `.claude/skills/` relocates it by hand: confirm each file's bytes agree with the canonical copy or
move it under `.agents/skills/` by hand, then delete the `.claude/` content other than root `settings.json`. The gate
refuses retired mirrors by name until they are gone.

`ops github setup --action-version YY.M.D` writes and reconciles a confirmed Project repository's `ci.yml`, `cd.yml`,
and `navigator.yaml` version pin in one pull request, preserving all other manifest content. A retry repairs a partial
update on the same branch and reuses its pull request; once the pins and applicable canonical skills converge, another
run writes nothing. `--action-version` defaults to the release the running `navigator` reports as its own version — but
only when this binary can actually vouch for that version: a downloaded release binary, or one built with
`NAVIGATOR_RELEASE_TAG` set, both of which can only report a version this repository has already published. A plain
local build cannot make that promise: `[workspace.package].version` is bumped on `main` days before the matching tag
exists. It carries no default, so `--action-version` must be named explicitly. A value that is not an exact release tag
— including no value, when this binary cannot vouch for one — is refused before any file is written.

A Project repository carries no `apps/` and no root `portal/`. A Vite application in either place is a gate warning that
points at `pages:`; the matter's pages are declared in `navigator.yaml` and render on the matter show page.

`validate` accepts templates, applications, either, or both, and reports a repository carrying neither distinctly rather
than failing it. A Project may legitimately open before either half exists.

### The blueprint directory is flat

A Project repository keeps its [Drafts](glossary/draft.md) in `drafts/`. `templates/` is the alias spelling: the gate
accepts it with a warning that names the rename, `git mv templates drafts`. Files sit directly under the chosen
directory as `<code>.md`. Both directories present is an error, including when one is empty; neither is allowed when the
Project uses only shared catalog templates. The scaffold creates `drafts/` holding the onboarding letter's stub.

Two other content folders complete the layout:

- `notes/` holds call notes and memos while they are written. Each one feeds `navigator project communication create
  --body-file`; the [Communication](glossary/communication.md) is the record and its body lives in Navigator's store.
  The scaffold's root `.gitignore` ignores `notes/`, and the gate refuses any `notes/` path Git would carry.
- There is no `notations/` folder. A [Notation](notation.md#notation) is a database record with its own workflow state,
  never a file. Each version exchanged with opposing counsel is a commit to the Draft on a pull request, and the
  counterpart's email is a Communication.

These files are separate from Navigator's shared `templates/notations/neon_law` and `templates/notations/forms` catalog.
N110 holds the shared catalog to those shelves and accepts direct Project files under either spelling. Navigator reads
the chosen directory at the pinned `main` commit, validates the notation contract, persists the bytes as a
content-addressed Asset, and records the imported commit SHA and the body's SHA-256 on the Template version. The store
refuses a Project version without either. A Notation copies both when it opens, and its body renders only when the
stored bytes still match that digest. Moving identical bytes from `templates/` to `drafts/` preserves the existing
Template version and its first source commit.

The Project workflow board also reads the chosen blueprint directory from Navigator's configured bare repository at
current `main`. A blueprint appears before its first run, with its workflow states and a never-started row. Reading the
board does not import a Template or open a Notation. Existing runs keep the workflow of their pinned Template version; a
revised blueprint appears beside them as a new never-started row. Repository read failures fail the board request
instead of presenting an empty board.

A Project references a shared catalog template by its existing `code`, without carrying a local copy. When no matching
`<code>.md` exists in the chosen Project blueprint directory, notation creation resolves the workspace-shared catalog
row and pins that exact template version on the Notation. A local file intentionally overrides the catalog for that
Project. Use one only for a genuinely Project-specific blueprint. Do not recode, rename, or copy a catalog template into
a Project repository merely to reference it. The `navigator project onboard` and `navigator project offboard` commands
are the exception: each creates a local letter stub on its first run, then opens that local template after it has been
drafted, committed to `main`, and the local copy matches `origin/main`.

The filename stem of a Project-local template carries no required Project-code prefix (ENG-693). A `template` row is
already scoped to its Project by `template.project_id`, so `code` only has to be unique *within* that one repository —
the layout gate enforces that with its own `declared_codes` check — not qualified by the Project's own code, which the
repository's `navigator.yaml` already declares. Frontmatter `code:` must still equal the filename stem exactly; the
layout gate refuses a mismatch. A repository that adopted the old `<project-code-with-underscores>__<name>.md`
convention (ENG-341) keeps it: dropping the requirement is not a rename, and no existing `code` — some of which are
already recorded as provenance against executed instruments — moves or changes.

## An individual client's entity

A Project's `entity_id` is `NOT NULL` (see [the glossary](glossary/project.md)): every matter opens against an
[Entity](glossary/entity.md), never directly against a [Person](glossary/person.md). A company client can reuse its
existing Entity. For a solo natural-person client with no company of their own, `navigator project scaffold` creates a
`Human` Entity named for the client and validates the client's home jurisdiction before writing it, never the firm's or
an assumed jurisdiction. The command may also reuse an existing Entity by name. `seeds/Entity.yaml` declares a `Human`
entity's jurisdiction under its `entity_type:` block, and the import route also refuses a record that omits it.

## A counterparty is a reference, not a row with access

A counterparty a matter verified — the opposing organization, its officers, its registered agent, the individual who
signed the instrument — reaches a row from a Project repository only as a [Project
Reference](glossary/project-reference.md), through `seeds/ProjectReference.yaml`. Never through `seeds/Person.yaml` or
`seeds/Entity.yaml`: a Person seeded under a Project-scoped session is linked to the Project as a participant, which is
the portal, and a scoped session cannot create an Entity at all (`ScopeViolation::EntityCreateNotScopable`). The
reference model is the one route through which a scoped session creates an Entity, and only as the referenced party; the
Project's own `entity_id` never moves.

```yaml
lookup_fields:
  - project_id
  - reason
records:
  - reason: counterparty_entity
    entity:
      name: Widget Works LLC
      entity_type:
        name: LLC
        jurisdiction:
          name: Nevada
  - reason: registered_agent
    person:
      email: agent@example.com
      name: Agent Example
```

A record names exactly one of `person` or `entity`; a scoped session references from its own Project, and an operator's
unscoped import names it with `project: { code: <code> }`. A reference grants nothing: no participation row, no lens,
and no row in any party list — the participation ledger the portal reads and the matter workbench's "Project people"
both omit it by test. What it gives the firm is a node the [Conflict-Check Graph](glossary/conflict-check-graph.md) can
collide a future matter with. Adversity is then recorded against the referenced party with `navigator project adverse
add`, which calls `POST /app/api/projects/{id}/relationships/adverse`.

A dry run is only as good as its session. `navigator site import --dry-run --as-ci` plans under the scoped-session rules
CI carries, for the Project this checkout's `navigator.yaml` names, so a seed CI cannot perform fails on the operator's
machine as the same `403 scope_violation` the pull-request run reports. The reusable gate's `seeds` job runs that scoped
dry run on every pull request with a reachable host, so the violation never reaches `main`.

## Open and scaffold a Project

Use the current Navigator CLI release from an empty destination:

```bash
navigator project scaffold --dir <directory> [--host <host>] [--no-repository]
navigator project scaffold --dir <directory> --resume <code> [--no-repository]
```

The interactive scaffold gathers the Project code, client contact, Entity or jurisdiction, and conflict attestation
before it opens the Project. It reuses existing contacts and Entities, creates a missing client contact, and creates a
Human Entity only after validating the jurisdiction. A duplicate immutable code is refused. The CLI release emits the
canonical manifest, agent contract, workflows, license, CODEOWNERS, synced skills, the root `.gitignore` that ignores
`/documents/` and `notes/`, and `drafts/onboarding__letter.md` as an unedited stub, initializes Git, and validates the
generated repository. `navigator project onboard` refuses to open a notation from that stub until the letter is drafted.
Add any other Project-specific Draft or application content deliberately afterward.

Unless `--no-repository` keeps the Project repository-less, the scaffold then asks the server to create or adopt the
Project's source repository and prints each resource's outcome. If any step fails after the Project opened, rerun with
`--resume <code>`: it skips the prompts and the open, writes the tree into an empty directory or repairs the canonical
files of that Project's existing checkout, and retries provisioning. A directory holding another Project's scaffold is
refused.

With `NAVIGATOR_FORGE_BACKEND=forgejo`, provisioning creates a private `<firm-slug>/<code>` repository on the instance
at `NAVIGATOR_FORGEJO_ROOT_URL`, authenticated with the owning Firm's `forgejo_access_token` integration secret. An
existing repository is adopted only when Forgejo reports it private, in that Firm's namespace, and named for the code.
Missing settings are named together, and nothing falls back to another Firm's token or namespace. The repository URL and
`forge_provisioned_at` are written in one statement and never replace a URL another writer recorded;
`git_initialized_at` stays empty until source has actually landed. Unset, the backend is GitHub.

On Forgejo, the scaffold then sends the checkout as a [Headless Source Archive](glossary/headless-source-archive.md),
the repository's first commit. The same step runs on its own:

```bash
navigator project import-source <code> [--dir <checkout>] [--operation-key <key>] [--wait]
```

It reads the files Git would track, refuses the tree locally under the same rules the server applies, and uploads the
canonical ZIP to `POST /app/api/projects/{id}/source-imports`. The `SourceImport` DBOS schedule applies it when a
deployment sets `NAVIGATOR_DBOS_SOURCE_IMPORT_ENABLED=true`. The default operation key is the code and the tree digest,
so rerunning the same tree joins the same import. A Project whose source is already initialized, or whose repository
holds other source, is refused; history-preserving moves of existing repositories are a separate migration.

Then converge the other setup resources:

```bash
navigator project setup <code>
```

Rerun setup after a failure; it reports no `publisher` resource. The first main-branch push uses the repository's
generated `cd.yml` caller. `navigator project doctor --project <code>` checks local deployment and Project coordinates;
`navigator project gate` validates the repository.

### Proposing matter pages

From the checkout root, `navigator project pages init` prints a proposed `pages:` block (see [Matter
Page](glossary/matter-page.md)):

```bash
navigator project pages init
navigator project pages init --write
```

The proposal reads the pinned Project Skills (a litigation practice area suggests a deadline board and a discovery
cockpit; an estates or probate area suggests an estate documents page), an engagement Draft under `drafts/` (an
`engagement_billing_records` page carrying that Draft as `draft:`), and, with a stored login for the manifest's host,
the live matter: each source the gate's `Y023` check can count that holds rows adds the sections that render it. Without
a login the live input is skipped. Each lawyer lens carries the page's sections, each client lens only those its kind
permits a client to see, and every lens carries `boundary_note` and `provenance_statement`. The block passes `Y021` and
`D001` through `D005` before it is shown. `--write` adds it to `navigator.yaml` and leaves every other line as written;
replacing an existing `pages:` block needs `--force`.

A Project with a `matter:` block or a pleading Draft (`kind: pleading`) also gets a `matter_status_console` overview, a
`docket_deadline_board` docket page (`case_caption`, `deadline_board`, `docket_timeline`, `party_roster`), and a
`document_workbench` page for each pleading Draft, carrying that Draft as `draft:`. These pages declare the lawyer lens
only; add a client lens by hand. A litigation pin's deadline board takes the docket sections instead of a second board.

`navigator project pages kinds` lists every page kind with the sections it requires, may carry, and reserves to the
lawyer lens, read from the same skeletons the gate checks.

## Local checkouts

One checkout root per organization, holding one directory per Project code. The root is the organization's own name, so
nothing has to be translated between the coordinate and the path:

```text
~/neon-law/<project-code>
```

These are **source** roots. Git never stores legal files. Drive is an optional ingest dropbox read through its API, not
the working-file store, and no workstation mount of it is read.

## Verifying a machine

`navigator project doctor` reports whether this machine and one Project workspace actually satisfy the map above, before
anything is created:

```bash
navigator project doctor
navigator project doctor --project acme
```

It resolves the active deployment from `NAVIGATOR_GCP_PROJECT_ID`, then reports that deployment's Google Workspace, its
optional Drive ingest, the stored site login, and — with `--project` — that Project's Drive folder path.

The command is strictly read-only and makes no network or database call at all: the diagnosis is a pure function of an
environment lookup, the stored credentials, and a clock. Every missing deployment setting is named in one run. Drive
ingest is optional: with none of its settings, it is reported disabled as a warning and a Project needs no folder; with
some but not all, the report fails and names every missing key. A Workspace, Drive, or identity mismatch exits nonzero
rather than warning. An absent login is a warning. A deployment that cannot be resolved stops the report, because every
later coordinate would otherwise describe some other Workspace.

It is not `ops doctor`, which diagnoses scheduled-job health in a running Kubernetes namespace.

To create or adopt the Project surfaces and Firm-private provider resources after a failed or legacy open:

```bash
navigator project setup acme
```

The same surface pass runs best-effort when a matter opens. This command is the operator retry: it authenticates to the
logged-in deployment like every other `navigator site` command and asks its admin API to talk to Drive, the forge,
Slack, and Notion. Even against a local deployment, it never opens a database connection of its own — the site does that
work, not the CLI.

## Reconciling repositories against live rows

One `projects.code` names both a repository and a row, and nothing makes the two agree. A repository declares its code
in `navigator.yaml`; a row records its repository in `project.repository_url`. Either side can be written without the
other, and neither side complains.

Reconciliation is therefore two halves, in two places, because the two questions need different things:

| Question | Answered by | Needs |
| --- | --- | --- |
| Does this row agree with the repository it records? | [the reconciliation door][door] | One row and a rule |
| Does this repository's declared code name a live row? | `navigator project drift` | The checkouts on a machine |

[door]: #reconciling-the-rows-against-what-they-record

The row half belongs on the server because a row can be checked against itself: a Project code *is* its repository name,
so a row whose URL names a different repository is drift with no checkout involved. The repository half cannot be: only
a machine holding the clones knows what repositories exist. This section is the second half.

```bash
navigator project drift --dir ~/<organization>
navigator project drift --dir ~/<organization> --all --json
```

It reads every checkout directly under `--dir` — the [local checkout root](#local-checkouts) — and takes the live
Project codes from `project_codes` on the reconciliation report. The codes rather than the findings, because a row that
is entirely fine produces no finding: an absence in the finding set would mean "reconciled" and "does not exist"
indistinguishably, and the whole question here is which codes exist.

Deliberately not `GET /app/api/projects`. That route returns the *caller's* matters — `store::access::visible_projects`
scopes to participation rows for every firm tier, Owner and Admin included — so a repository whose row exists but the
caller does not participate in would read as a repository with no row at all, which is the loudest finding this command
emits. The reconciliation door is admin-tier and reads every row.

| Finding | Meaning | Severity |
| --- | --- | --- |
| `repository-has-no-row` | Declares a code no live row carries; its Matter Pages render nowhere | fail |
| `duplicate-code` | Two checkouts claim one Project code. | fail |
| `unreadable-manifest` | `navigator.yaml` is present but unparsable, or names an invalid code. | fail |
| `manifest-disagrees-with-name` | Declares a code other than the repository's name. | warn |
| `no-manifest` | The checkout declares no Project, so it cannot be reconciled. | warn |
| `rowless-by-declaration` | The repository declares it is meant to have no row. | counted, never failed |

Warnings do not make a fleet drifted, in the same sense `projects doctor` uses. It is strictly read-only on both sides:
it creates no row, patches none, and closes none — reconciling a repository to a row is a decision about a matter.

Exit codes are three values rather than two, because a gate reads them:

| Code | Meaning |
| --- | --- |
| `0` | Every repository reconciles. Warnings do not change this. |
| `1` | At least one failing finding. |
| `2` | The report could not be produced — the scan root, the login, the host, or the response. |

The split between `1` and `2` is the one that matters: a gate treating "drifted" and "could not ask" alike goes green on
an expired token. `--json` carries the same answer as `reconciled`, and each finding serializes with its own fields
beside its `kind` and `severity`, so a consumer reads the repository or code it needs by name rather than parsing the
`detail` sentence.

### A repository declares its own absence

Not every repository without a row is drift. A closed matter may deliberately carry no row, and a command that reports
known-good repositories as failures is a command nobody runs twice. The repository says so in the manifest it already
carries:

```yaml
host: staging.neonlaw.com
project: acme
no_live_row: the matter closed in March; no row was opened
```

The value is the reason, not a boolean, because a boolean records that someone silenced a finding without recording why.
`no_live_row: true` is refused as a manifest error rather than honored as a suppression.

The suppression cannot live in Navigator's own source. A Project code *is* a client identifier — it names who retained
the firm — and this repository is public, so a constant list of the codes to skip would publish exactly what
[`AGENTS.md`](../AGENTS.md) forbids. A `--ignore` flag only moves that list into a runbook or a CI invocation, where it
is written down just the same and reviewed less. Inferring intent from shape — no `portal/`, no `seeds/`, so nothing is
load-bearing — is worse than either: an empty repository is also what a brand-new *unreconciled* Project looks like, so
that rule would go quiet about precisely the gaps the command exists to find.

This is the opposite of the call the allowed-root list in `cli::projects::repository` makes, and deliberately so. That
list governs a rule identical for every repository — which paths may sit at a root — so centralizing it costs nothing
and keeps the gate from going advisory. Whether one matter is meant to have a row is a per-matter fact only that matter
knows.

Declared row-less repositories are still counted in the footer, and `--all` lists them. Suppressed and silent are
different things: a report that hides repositories without saying so fails the same way as one that cries wolf.

## Shared notations

The notations that are not specific to one Project live in **this** repository, under `templates/notations/neon_law/`
and `templates/notations/forms/`. They are Navigator's own catalog, versioned and validated with its source.

A Project repository's `drafts/` directory (or its `templates/` alias) is separate from that catalog. It carries the
blueprints belonging to that Project, and Navigator imports each one at the commit it reads. A Project-local notation
may record its lineage in a `derived_from` frontmatter field, which is documentation — the rule engine does not resolve
it, so it creates no dependency on any other repository being present.

**There is no cross-deployment template repository, and a Project repository must not assume one.** Everything a
Project's notations need is either in that Project's own blueprint directory or in this repository's catalog.

## Source boundaries

`neon-law/navigator` is Navigator's source repository. It is not a Project repository. Each Project's repository is its
own deployment-specific source repository.

A Project repository may hold notation templates, fixtures, tests, and the checked-in configuration required to build or
validate them. It may not hold client uploads, answers, generated legal documents, secrets, dependencies, or build
output. An imported Word document's Notation Markdown source and its block/anchor manifest are matter work product under
this same rule — they live in internal, content-addressed Assets
([`store::notation_documents`](../store/src/notation_documents.rs); see [Notation document
source](notation.md#notation-document-source)), never in a Project repository or any other git history.

## Access boundary

Navigator Project participation authorizes Navigator and the matter show page. It never grants source-forge access.
Outside lawyers work through Navigator and Drive without any membership of the forge that hosts the source. Repository
access is an independently administered source-control decision.

This is also why the model stops at one repository per Project rather than one repository per organization with
`projects/<code>/` subdirectories, which would be the same logic one step further. **Repository access is the per-matter
access boundary**: a per-organization monorepo would hand every contributor every matter's source. One repository per
Project code is the floor.

## Implementation boundary

This contract deliberately leaves deployment provisioning, access reconciliation, and migration to their own
implementations. Those changes must preserve these authorities and may not reintroduce legal files into Git.
