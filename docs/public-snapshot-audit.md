# Public snapshot audit

Navigator publishes from a fresh root commit, so whatever the tree tracks at that moment becomes the public baseline.
Rule `P001` checks regular files in the Git index against fixed credential, contact, cloud-coordinate, secret-file,
build-output, and image/PDF metadata patterns. It holds findings to the rule for [what may be
public](public-contributor-safety.md#what-may-be-public): source, documentation, and verified public firm facts. It does
not decide whether text is confidential, whether a fact is genuinely public, who owns an asset, or whether anyone has
permission to redistribute it. Synthetic fixtures may also be public.

It runs as rule `P001` inside `navigator validate` and `navigator project gate`, so CI's `gate` job runs it on every
pull request. A tree opts in by carrying the [allowlist](../.github/public-snapshot/allowlist.toml) at
`.github/public-snapshot/allowlist.toml`; a Project repository or any other directory without that file is never
audited.

```bash
cargo run -p cli --quiet -- validate
```

## What it refuses

In a Git work tree, the audit reads regular files returned by `git ls-files --cached`. It includes staged additions and
excludes untracked files, Git history, untracked build outputs, and deployed or packaged artifacts. Outside a Git work
tree, it reads regular files below the root except files under `.git/`. It skips the audit's own allowlist file. A read
error fails the audit.

- `credential`: a private key block with a key body; an AWS, GitHub, Slack, Stripe live, Google API, or SendGrid key
  shape; a service-account key id; or a URL whose password is not a placeholder and whose host is not local or reserved.
- `email`: an address outside `example.com`, `example.org`, `example.net`, and the `.example`, `.invalid`, `.test`, and
  `.localhost` domains. Service-account addresses are checked as cloud coordinates.
- `phone`: a dialable North American number, except the `555` area code or exchange and `N11` service codes.
- `gcp`: a service-account address, a project number in a Workload Identity path, a `gs://` bucket, or a project id in a
  resource path, a registry path, a `gcloud` flag, or a `*_PROJECT`, `*_PROJECT_ID`, `*_BUCKET`, or `*_SERVICE_ACCOUNT`
  variable.
- `env`: a `.env` file other than `.env.example`, `.env.sample`, or `.env.template`; a credential file name or key-store
  suffix; a `.decrypted.` file; or a plaintext value in a `*.enc.yaml` or `*.sops.yaml` document.
- `build`: a path under `target/`, `node_modules/`, `dist/`, `build/`, `.venv/`, `venv/`, `__pycache__/`, or `.next/`;
  a `.DS_Store`, `Thumbs.db`, or `lcov.info`; or a compiled object or bytecode suffix.
- `binary-metadata`: a PNG `eXIf`, `tEXt`, `zTXt`, or `iTXt` chunk; a JPEG `APP1`, `APP13`, or comment segment; a WebP
  `EXIF` or `XMP` chunk; or a filled PDF `/Author` or XMP `dc:creator`.
- `binary`: a binary format the audit cannot inspect for metadata. Font formats are excluded from this finding; this
  rule does not verify font provenance, license terms, or notice delivery.

A cloud name is a placeholder when it is shorter than six characters, contains `example`, `placeholder`, `invented`,
`fake`, `dummy`, `synthetic`, `fixture`, or `sample`, or starts with `my-`, `your-`, `proj-`, or `test-proj`. Google's
public image projects such as `distroless` also pass.

A finding prints the file, line, class, and detector description. Detected values are never printed. The `gitleaks` job
and the [PII scan](public-contributor-safety.md#the-pii-scan) run beside this rule. Their configured scans and `P001`
cover different patterns; none establishes ownership, legal permission, notice completeness, or that a release archive,
extension package, container image, or deployed asset matches the checked-in source. Review each output and its notices
separately before redistribution.

## Resolving a finding

Replace the material with a synthetic value first: a reserved email domain, a `555` number, or an `example-` cloud name.
Strip metadata from an image without re-encoding it by dropping its text and EXIF chunks.

Allowlist only what the tree keeps on purpose: a firm public contact, a third-party attribution a licence requires, a
domain-shape fixture the behaviour under test needs, or a coordinate another change is already removing. Each entry
names its `class` and `reason`, and narrows itself by `paths` (exact paths, or prefixes ending in `/`), by `match` (a
regular expression the whole value must match), or both. An entry that permits nothing prints a `P001` warning; delete
it.

```toml
[[allow]]
class = "email"
match = '''release-train@users\.noreply\.github\.com'''
paths = [".github/workflows/release-cut.yml"]
reason = "GitHub's noreply commit identity for the release bot, not a mailbox."
```

An allowlist entry suppresses only the matching automated finding. It does not prove the material is public or licensed.
A real person's contact data, a credential, or a deployment coordinate never belongs in the allowlist. This audit does
not clear imported templates, government forms, artwork, fonts, or third-party code; track that evidence in the
[redistribution audit](licensing/redistribution-audit.md). Verify packaged artifacts against their source and notices at
the release candidate.
