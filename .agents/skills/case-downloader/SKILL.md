---
name: case-downloader
description: >
  Record exactly which cases and statutes a matter relies on: verify each authority against a primary source, download
  the original, file it as an Authority with `navigator project sync`, and commit only the YAML pointer. Trigger
  whenever a matter needs the text of a case, statute, or regulation saved, cited, or checked, and before quoting one in
  work product. Kept identical, byte for byte, in every Project repository's `.agents/skills/`; this canonical copy
  lives in Navigator's own `.agents/skills/`.
---

# Case downloader

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

A matter repository records which authority it relies on. The bytes live in object storage as an Authority; Git holds
only the pointer. Never commit an opinion, statute, or any other original to Git.

## 1. Verify before downloading

Download nothing you have not verified.

- **A case:** confirm it with Descrybe. Pull the passages you intend to rely on and the case's treatment (whether later
  courts overruled, limited, or followed it). Record the citation exactly as the source states it.
- **A statute or regulation:** confirm it with Midpage `analyzeLaw` and check `isCurrent`. A section that is not
  current is not filed as current law; record the version you checked and its date.

If a source cannot confirm the authority, stop and tell the lawyer. Do not file from memory.

## 2. Download the original

- **A case:** Descrybe `get_case_pdf` returns the opinion as a PDF.
- **A statute:** save the official publisher's HTML, such as the state legislature's own site, not a third-party
  copy.

Write a `SHA256SUMS` file beside the originals in the matter's Drive folder so anyone can re-check them.

## 3. File the Authority

Stage each original under the Git-ignored `documents/`, in the folder that names its class, with a sidecar beside it:

| Class | Folder |
| -- | -- |
| `case_law` | `documents/cases/` |
| `statute`, `regulation`, `administrative`, `secondary` | `documents/rules/` |

The sidecar is `<file>.authority.yaml`:

```yaml
class: case_law
citation: "410 U.S. 113 (1973)"
title: Roe v. Wade
canonical_url: https://example.test/roe
checked_on: "2026-09-05"
```

`short_cite`, `publisher`, and `issued_on` are optional. A capture without a sidecar, or one whose `class` belongs in
the other folder, is refused.

Preview, then file it:

```bash
navigator project sync --dry-run
navigator project sync
```

To limit a filing run to one folder, pass `--only documents/cases` or `--only documents/rules` to both commands.
`project sync` archives the bytes as a global Authority, writes the pointer `blobs/cases/<file>.yaml` or
`blobs/rules/<file>.yaml` carrying its `authority_id`, removes the staging sidecar, and keeps the bytes in `documents/`,
ignored by Git. An exact citation with the same archived bytes reuses its global Authority; different bytes under that
citation require review. Equal bytes alone do not establish identity. To correct a field on an Authority already filed,
use `navigator site authorities update`. To file one without a checkout, use `navigator site authorities create`.

## 4. Commit the pointer only

Run `navigator project gate` before pushing; it refuses tracked raw bytes and staging sidecars. Commit only the `.yaml`
pointer under `blobs/`; the root `.gitignore` keeps everything under `documents/` out.

A fresh clone gets the bytes back with `navigator project sync`, which verifies each against the pointer's `sha256`.

Do not file an Authority a second time as a `kind: exhibit` copy under `documents/exhibits/`. Use `exhibits/` only for a
document the matter itself produced or received.

## 5. Court notices are not authorities

A CM/ECF Notice of Electronic Filing is the matter's own docket, not an authority. Do not parse one by hand or spend its
free-look link from a browser. Save the notice as an `.eml`, list the case under `dockets:` in `navigator.yaml`, and
run:

```bash
navigator project docket ingest notices/*.eml --dry-run
navigator project docket ingest notices/*.eml
```

The verb downloads each document once and files it as `documents/Docket Filings/DKT NNNN - <title>.pdf`, with its
Pointer under `blobs/`. It records the Service List as Persons and Relationship Log rows for conflicts without adding
those people to the Project.
