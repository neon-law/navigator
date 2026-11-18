# Navigator CLI

The `cli` crate builds `navigator`, the application command a lawyer's Mac and every Project repository's CI install.
It handles Projects, Jujutsu repositories, validation, site sessions, People and Firm administration, mail, Inbox,
Downloads, Legal Plans, and local draft rendering. Its rules live in the crate's library. Connection-free domain types
come from `model`; rendering uses the shared PDF and Word engines, including Typst. The binary links no database driver,
cloud SDK, web client, or durable workflow runtime.

`hudson` (the `deploy` crate) is the operator CLI for local development (`dev`) and production operations (`ops`). It
retains hidden application compatibility commands during the transition. `scorpio` is the Ephemeris CLI. Each binary
builds in its own release job.

The local staging lifecycle (`hudson dev staging`) runs under the `NAVIGATOR_ENVIRONMENT=dev` application profile;
staging is a lifecycle target, not an application environment.

`navigator mail` searches, reads, drafts, and files matter email. `navigator mail sync --project <code> --host <host>
--kind letter --watch` files labeled conversations and sent follow-ups without sending email. Gmail client-secret
provisioning belongs to `hudson ops gmail-client`.

Run `navigator --help` for the current command surface. Use [AGENTS.md](../AGENTS.md) for the local development loop,
[agent workflows](../docs/agent-workflows.md) for repository work, and [cloud operations](../docs/cloud-operations.md)
for deployment procedures.

## Common workflows

Log in to a deployment once; the CLI stores the short-lived bearer token locally:

```bash
navigator site login --host staging.neonlaw.com
```

To discover the repository and Drive coordinates derived from a Project, run the read-only Project check with its code:

```bash
navigator project doctor --project <project-code>
```

Check this repository locally. The gate walks the whole tree from the root it is run in, fixing what is safe to fix and
reporting what needs a person:

```bash
navigator project gate
```

Lint an arbitrary directory that is neither a Navigator checkout nor a Project repository:

```bash
navigator validate
navigator validate /path/to/tree
```

Open a matter through the logged-in site, against a pre-existing client and either an existing entity or a `Human`
entity this command creates for a solo client:

```bash
navigator project create --name "Acme LLC — Formation" --code acme-llc-formation \
  --client-email <client@example.com> --entity-name "Acme LLC" --attest
navigator project create --name "Shook Estate" --code shook-estate \
  --client-email <client@example.com> --jurisdiction Nevada --attest
```

Create a notation on an existing Project through the logged-in site. Each Project keeps one onboarding notation and one
offboarding notation; later work uses other kinds. The shared catalog codes are `onboarding__letter` to open and
`offboarding__letter` to close:

```bash
navigator site notation create onboarding__letter \
  --project <project-code> \
  --client-email <client@example.com>
navigator site notation create offboarding__letter \
  --project <project-code> \
  --client-email <client@example.com>
```

Before opening another instrument, inspect the matter's private notation inventory. A lawyer can then read the filed
answers and their source provenance for a particular Notation:

```bash
navigator site notation list --project <project-code>
navigator site notation answers <notation-uuid>
```

Both read matter content, so both require a firm-side participation row on the matter — of every tier, Owner and Admin
included. Being Owner or Admin is not itself a key to a matter's work product: seat yourself on the matter first, or
these answer `404`. A Clerk is not lawyer tier and gets `403`.

Before cutting a release, check that this repository's self-referencing GitHub Actions pins still name
`[workspace.package].version`. The reusable workflows and composite actions under `.github/` reference this repository's
own actions by an absolute tag rather than by the ref the caller used, so a bump that does not sweep them publishes a
gate no consumer can run:

```bash
hudson ops release pins
```

It takes no version — the manifest is the answer. It walks `.github/` and `docs/examples/`, exits `0` when every pin
agrees, and exits `2` naming the file and line of each one that does not. Comments and the named placeholders
(`@YY.M.D`) are excluded; every other ref is a pin, including one that is not a release version at all. `ops release
version`sweeps the pins as it writes the manifest, so this verifies that bump rather than replacing it;`ci.yml` runs the
same command in its always-run gate.

## Exit codes for `--ci` commands

`navigator site import --ci` and `navigator project gate --check --ci` first exchange the runner's GitHub Actions OIDC
token for a Navigator session at `/auth/ci/seed-token` or `/auth/ci/document-token`, then run the command's own gate.
Those are two different kinds of failure, so they exit differently:

- **`2`** — the ordinary gate failure: the mint succeeded and the command's own check found a problem (a reconciliation
  error, a document whose stored object is missing or corrupt, and so on).
- **`3`** — the mint itself was refused before any gate ran (unauthorized, forbidden, or the deployment is
  unavailable). The refusal also prints a GitHub `::error::` workflow-command annotation naming the door
  (`/auth/ci/seed-token` or `/auth/ci/document-token`) and the server's own message, so the Actions log shows why the
  run was refused rather than a generic HTTP status.

## Exit codes for `navigator site import`

`navigator site import` posts a large seed document in chunks of 50 records, so a proxy timeout on one request cannot
hide what the server committed. When a chunk answers with a gateway or server error, or no answer at all, the CLI
re-sends that chunk as a dry run. If every record now reads unchanged, the chunk committed after all. Otherwise the
import stops there and prints `committed N of M records`.

- **`0`**: every record is committed.
- **`4`**: some records are committed and a later chunk is not. Re-run the import; committed records read unchanged.
- **`2`**: nothing was committed, or the document was refused.
- **`3`**: with `--ci`, the token mint was refused (see above).

## `drafts` and `templates` are `notation`, spelled the way you reach for it

`navigator drafts` and `navigator templates` are visible aliases of `navigator notation`: the same group and dispatch.
Use whichever noun fits.

## Previewing an uncommitted draft

```bash
navigator notation preview templates/notations/neon_law/onboarding__letter.md --no-open
```

This renders a local review PDF from the file under your cursor, even before a source commit exists. Use `--out` to
choose its location and `--answer` or `--answers-file` for questionnaire answers. Without `--no-open`, the command opens
the PDF. It creates no Notation, workflow instance, or source edit.

The hidden `hudson notation preview` compatibility command retains the older live questionnaire preview. Local Matter
Page preview remains in Hudson until its reads can use the site API.

## Rendering a template to PDF or Word

```bash
navigator notation pdf templates/notations/neon_law/onboarding__letter.md --out /tmp/onboarding.pdf --preview
navigator notation word templates/notations/neon_law/onboarding__letter.md --out /tmp/onboarding.docx --preview
```

Both validate the file against the same rule set as `navigator project gate` first, resolve the render frame from the
template's own `kind:`/`output:` frontmatter, fill any `{{code}}` placeholders passed with `--answer`, and compile the
result in pure Rust. See [`pdf/README.md`](../pdf/README.md) for the render pipeline and output formats.

You do not need a site to work locally. Use `navigator project gate`, the `navigator notation` authoring commands, and the
KIND-backed `hudson dev` loop; seed a local catalog with `hudson site seed` when that command's local store and storage
environment are available, or import deployment data with `navigator site import` after logging in.
