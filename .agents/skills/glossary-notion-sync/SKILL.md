---
name: glossary-notion-sync
description: >
  Sync [`docs/glossary/`](../../../docs/glossary/README.md) (one Markdown file per term) with its mirror page in Notion,
  in either direction. Trigger when asked to push the glossary to Notion, pull a Notion glossary edit back into the
  repository, or refresh the Notion copy after a term changes. The repository is the source of truth; Notion is where a
  colleague reads and proposes. A pull is a pull request, never a paste.
---

# Syncing the glossary with Notion

The two copies are not peers:

- **[`docs/glossary/`](../../../docs/glossary/README.md) is the source of truth.** One file per term, with a
  one-sentence plain-text `description:` in frontmatter, reviewed in pull requests, embedded in the binary through
  `store::glossary::GLOSSARY`, materialized into `glossary_term` rows on every boot, published at `/glossary`, and read
  by `navigator glossary list` / `navigator glossary show <term>`. A definition is not real until it lands here.
- **The Notion page is the reading and proposing surface** for colleagues without a checkout. Find it with a Notion
  search for the page titled **Glossary**; its URL is a private firm coordinate and stays out of this repository. Every
  push rewrites it wholesale, so an edit made there survives only until the next push unless someone pulls it into the
  repository first.

Say so when handing someone the page, so nobody mistakes the overwritten copy for the glossary.

## The mechanical contract

`store::glossary::terms` is the reader of record for both directions:

- **Every `<slug>.md` beside the README is one term.** Its frontmatter carries `title:` (the term as a reader says it)
  and a one-sentence plain-text `description:`; its body is the definition. `store::glossary::parse_entry` refuses a
  file whose name is not the slug of its title.
- **The slug is a public API.** `store::glossary::slugify` turns `Lawyer Review` into `lawyer-review`: the file name,
  the `/glossary#lawyer-review` anchor, and the `glossary_term.slug` row key. `docs/*.md`, `store/src/projects.rs`,
  `mcp/src/tools/*.rs`, and `portal/src/api.rs` link `glossary/<slug>.md`. Renaming a term breaks every one of them, so
  grep before renaming and fix the callers in the same commit.
- **Terms link each other as siblings:** `[Matter](matter.md)`. The web page turns that into `#matter`. A repository
  path climbs two levels (`../../store/`), a contributor doc one (`../notation.md`). `M057` checks every link resolves
  on disk; `M061` checks the page can render it.
- **There is no index to maintain.** The page's side navigation and the CLI's `list` derive from the files.

## Pushing: repository → Notion

Generate the page; never hand-edit it.

```bash
cargo run -p cli --quiet -- glossary notion > /tmp/glossary-notion.md
```

The output joins every term into one page (`## Title` then body, alphabetical by slug), resolves repository paths to
`blob` and `tree` URLs on `main`, reduces sibling-term links to their label (Notion has no heading anchors), unwraps the
120-column hard wrap, and prepends a provenance blockquote stating the sync direction.

Replace the page with it:

1. Split the output at `##` heading boundaries into chunks of roughly 24 KB. The whole document (about 120 KB) will
   not fit in one call.
2. `notion-update-page` with `command: replace_content` and the first chunk.
3. `notion-update-page` with `command: insert_content` and `position: {"type": "end"}` for each remaining chunk, **in
   order**, one at a time. A parallel insert scrambles the page.

Verify with `notion-fetch` on the page URL. Its heading count must match the number of lines `cargo run -p cli --quiet
-- glossary list` prints, and the last term must be Workshop.

## Pulling: Notion → repository

**A pull is a pull request.** Fetch, read, and hand-apply each change to the term's own file. Never paste the Notion
page over the directory: it has absolute GitHub URLs, flowed paragraphs, and no links between terms.

1. `notion-fetch` the page and diff it against `cargo run -p cli --quiet -- glossary notion` output. Ignore Notion's
   own re-serialization: it returns a Markdown table as a native `<table>` block and drops blank lines between blocks.
   What remains is what somebody typed.
2. **Read every difference before applying it.** Notion holds the firm's real matters, and this repository is public.
   A definition from Notion may name a client, a matter, or a Project code. The no-client-data rule in
   [`AGENTS.md`](../../../AGENTS.md) governs the destination: rewrite the example as a mechanism ("a Project's publish")
   or use a synthetic code. If a real code is in the Notion text, say where and let a human decide.
3. Apply the wording to `docs/glossary/<slug>.md` in its own idiom: sibling links, hard wrap at 120. A new term is a new
   file named for the slug of its `title:`.
4. Run the gate:

   ```bash
   cargo run -p cli --quiet -- project gate
   rtk cargo nextest run -p cli -p store -p portal
   ```

   `store::glossary::GLOSSARY` is `include_dir!`, so a glossary edit changes compiled data and boot-time rows. The store
   tests are required.
5. Open the pull request, then push the merged text back to Notion so the copies agree.

## Rules

- **Describe terms in prose.** When a table matters, link to its `DEFINE TABLE` statement in
  `store/src/schema/navigator.surql` rather than copying its fields.
- **Don't rename a term to fix its wording.** Change the body; a slug change is a cross-cutting rename (grep
  `glossary/<slug>.md` first).
- **Don't add a second Notion page** for part of the vocabulary. One page mirrors one directory.
- **Don't paste Notion's text into the files.** Follow the pull procedure above.
- **Don't push from a dirty or unrebased worktree.** The page claims to be generated from `main`; generate it from a
  checkout current with `origin/main`.
