---
name: markdown-lint
description: >
  Lint every `.md` file in the workspace with the navigator CLI (M-family rules plus the S101 120-character line limit).
  Trigger when adding or editing any Markdown file (READMEs, `docs/`, `AGENTS.md`, blog posts under `server/content/`)
  and before committing `.md` changes. Use the workspace's own binary; never hand-roll a different linter.
---

# Markdown linting via the navigator CLI

Lint is not a confidentiality classifier. Before validating a public example, read
[`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md) and remove client data, legal files,
real contact details, and production identifiers.

## The command

Run it from the repository root; the gate takes no path and refuses to run anywhere else:

```bash
cargo run -p cli --quiet -- project gate
```

It classifies each file by content and path:

| File | Rules |
| --- | --- |
| Prose Markdown, including a plain README | M-family, S101, S102 |
| Notation template under `templates/`, or any file declaring `questionnaire:` or `workflow:` | Adds the N-family |
| Event | E-family |
| Blog post | C-family |

One run covers the whole workspace, including the typed event pass and a `.yaml`/`.yml` parse. `.agents/` stays in
scope; `.git`, `.build`, `.claude`, `.codex`, `node_modules/`, `dist/`, and `target/` are skipped. The per-kind
frontmatter keys are documented in `docs/frontmatter.md`.

The gate fixes trailing whitespace, ATX heading spacing, blockquote spacing, and S102 paragraph packing in place, so
what prints still needs a person. Exit `0` means clean; otherwise each violation prints to stdout with file, line, rule
code, and message. Run it before committing any `.md` change; CI also validates the content tree.

## Common rules that fire

- **S101**: line longer than 120 characters. Reflow the paragraph.
- **M026**: heading ends with `.`. Drop the period. Bash `# comment.` lines inside fenced code blocks trip the same
  rule.
- **M038**: inline code span has leading or trailing whitespace, usually because it broke across two lines. Keep code
  spans on one line.
- **M040**: fenced code block has no language tag. Add one (`bash`, `rust`, `text`, `yaml`, …) after the opening fence.
- **M031**: fenced code block needs a blank line before it, often inside a list item.
- **M060**: table column alignment is inconsistent.

## Don't

- Don't reach for `markdownlint`, `mdformat`, or any non-Rust linter. See [[rust]] for the Rust-only stance.
- Don't disable a rule by editing `cli/src/main.rs`. If a rule is wrong, fix it in `rules/src/<code>.rs` with a test.
