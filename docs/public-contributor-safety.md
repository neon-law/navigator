# Safe public experimentation

Navigator is public so people can read it, run it, fork it, prototype quickly, and build portal experiences without
turning a code repository into a matter system. Fast iteration is welcome because the boundary is clear.

> **Prototype freely. Publish only source.**

## What may be public

Commit source code, documentation, tests, design experiments, and fixtures that are either firm-owned or synthetic. For
any non-firm email address, use `example.com` or a reserved `.example`, `.invalid`, or `.test` domain. Do not include
phone numbers.

## What may never be public

Never put any of the following in Git, pull requests, commit messages, issues, planning tools, agent transcripts, or
other external planning surfaces:

- client or matter data, including party names, matter codes, addresses, contact details, answers, or work product;
- legal files, document bodies, uploads, generated documents, or screenshots containing their contents;
- production identifiers, credentials, hosts, project names, buckets, service accounts, or operational coordinates; or
- the product roadmap — Linear issue titles, project, initiative, milestone, or cycle names, and `linear.app` URLs,
  whose path carries the issue title as a slug. The roadmap lives in Linear; a published branch, PR, or commit
  references it by bare issue identifier (`ENG-1234`) and nothing more. See [Linking a PR to its Linear
  issue](agent-workflows.md#linking-a-pr-to-its-linear-issue).

Do not work around this rule by redacting a copy, using a realistic placeholder, or moving the material to another
third-party service. Client data and legal files belong in Navigator-managed systems and the firm's approved file
stores, never source control or external planning surfaces.

## A safe fast loop

1. Start from a synthetic fixture or a blank design.
2. Build and iterate locally; use the repository's local-development and test guidance.
3. Keep examples abstract or synthetic when describing a scenario to another contributor.
4. Validate the changed files and run the relevant tests before sharing the source.

For the Project-repository contract, including Navigator-managed reads and writes, see
[`project-repositories.md`](project-repositories.md). For the repository workflow and its no-client-data gate, see
[`agent-workflows.md`](agent-workflows.md#no-client-data-in-the-repo). The gate is proof for its scanned paths, not
permission to place sensitive material somewhere it does not scan.

## Seed layers and public firm facts

Every file under `store/seeds/` ships in the binary and seeds a database, so each row is either a public firm fact,
nonidentifying reference data, or conspicuously synthetic. Nothing else belongs there; private firm and client
configuration is runtime data supplied to a deployment, never a committed seed.

| Layer | Path | Applied | Holds |
| --- | --- | --- | --- |
| Canonical | `store/seeds/*.yaml` | every boot, every profile | reference catalogs and the public firm facts below |
| Brand | `store/seeds/neon/` | every boot of that brand | the published postal box, public handles, Posts |
| Sample | `store/seeds/sample/` | a `dev` profile only | synthetic matters' mail, answers, repository and roles |

The profile is the whole predicate for the sample layer. An unset or empty `NAVIGATOR_ENVIRONMENT` resolves to
`production`, and `NAVIGATOR_SIMULATED_MATTERS` only announces simulated matters; it writes nothing. A production boot
therefore seeds no Project, Letter, Answer, sample Person, or sample repository row. A binary test fixture lives beside
the test that uses it, never in `store/seeds/`.

### Public firm facts

Re-verify a fact against its source before changing a row; a fact with no public source is removed or supplied
privately.

- Shook Law PC, a California Professional Corporation (`Entity.yaml`): the firm website footer and `LICENSE`.
- Nick Shook, name (`Person.yaml`): the California, Nevada, and Washington bar directories.
- Nevada Bar No. 13400 (`Credential.yaml`): [nvbar.org](https://nvbar.org/find-a-lawyer/?usearch=13400), verified
  active 2026-10-14.
- California Bar No. 337252 (`Credential.yaml`):
  [calbar.ca.gov](https://apps.calbar.ca.gov/attorney/Licensee/Detail/337252), verified 2026-10-14.
- Washington Bar No. 63446 (`Credential.yaml`): the WSBA legal directory, verified active 2026-10-14.
- Reno box `5150 Mae Anne Ave Ste 405-9002`, 89523 (`neon/Address.yaml`): the firm website footer.
- `nick@neonlaw.com` with the admin role (`Person.yaml`, `User.yaml`): the firm-domain login that keys the
  administrator role; it carries no client data.
- `NeonLawUSA` and the Vesta handles (`neon/SocialHandle.yaml`): the profiles themselves.
- Posts (`neon/Post.yaml`): published on the firm website.

A trust, a personal entity, a further corporation, or another box is not a verified public firm fact and does not seed.

### The gate

`cli/tests/no_client_data.rs` scans `store/seeds`, `templates`, and `server/content` for client contact data
(`NCD-EMAIL`, `NCD-PHONE`), operational coordinates and credentials (`NCD-OPS`), and, under `store/seeds` only, any file
it cannot read as text (`NCD-UNSCANNED`). Its negative tests plant invented markers in every shipped seed path and
require a finding. `store/tests/seed_boundary.rs` pins the layer layout and the public-fact inventory above, and shows
that a production profile seeds nothing synthetic.

The gate does not scan source or test code, generated artifacts, or the rest of the tree, and an allowlist cannot prove
that a fact is public; the table above is that proof, kept by review.

### The public snapshot audit

Rule `P001` in `navigator validate` reads every tracked file, source and binaries included. It refuses credentials,
non-reserved email addresses, phone numbers, Google Cloud coordinates, secret-bearing files, build output, and embedded
image or PDF metadata unless `.github/public-snapshot/allowlist.toml` lists the finding with a reason. See
[`public-snapshot-audit.md`](public-snapshot-audit.md).

### The PII scan

The always-required `gate` job in `.github/workflows/ci.yml` runs `navigator pii` against a digest-pinned
[Presidio](https://github.com/data-privacy-stack/presidio) analyzer on loopback. Rust scans tracked prose and source
strings and comments. Presidio supplies name recognition through spaCy, plus email, phone, Social Security, ITIN,
credit-card and IBAN recognizers. Its Python runtime stays inside the upstream container; this repository needs no
Python scanner or pip installation.

A finding fails CI and reports only its file, line and category, never the detected value. Analyzer failures, missing
recognizers and a failed name-recognition canary also fail CI. Analyzer container logs are disabled. Replace findings
with synthetic data, or review an exception with a reason in `.github/pii/allowlist.toml`. Never allowlist private
client or contact information.

To run locally, start the pinned analyzer image from the CI workflow with its port bound to `127.0.0.1:3000`, then run:

```bash
cargo run -p cli -- pii --analyzer-url http://127.0.0.1:3000
```

This checks the tracked working tree, not Git history, deployed databases, CI artifacts, or text inside images and PDFs.
