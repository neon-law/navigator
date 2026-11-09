---
name: random-refactor
description: >-
  Review one tracked Rust file, chosen at random or named by the user, against workspace conventions, the standard
  library, and its siblings, then make at most one evidence-backed edit. Use for an untargeted quality pass.
---

# `/random-refactor`: one file, three comparisons, then the smallest honest edit

Read [`docs/public-contributor-safety.md`](../../../docs/public-contributor-safety.md),
[`docs/rust-programming.md`](../../../docs/rust-programming.md), and [`rust`](../rust/SKILL.md) first. Code is the
source of truth; comments, docs, tests, Presentations, and Workshops describe today; git holds rejected alternatives.
One pass over one file: not a sweep, and not a substitute for [`implement-issue`](../implement-issue/SKILL.md),
[`review`](../review/SKILL.md), or [`author-docs`](../author-docs/SKILL.md). This file must not grow.

## 1. Choose the file

If the user named a tracked `.rs` path, use it. Otherwise pick one tracked, hand-written, non-test Rust file; the
snippet runs under macOS bash 3.2 and zsh alike. Draw again when `/tmp/navigator-random-refactor/<file-stem>-*.md`
already exists: step 5's reports are the ledger.

```bash
git ls-files '*.rs' | grep -v -e '/tests/' -e 'build\.rs$' | sort -R | head -n 1
```

Record the path, the crate, and `rtk git log -1 --oneline -- <path>`. Refuse `target/`, bindgen blobs, and `OUT_DIR`.

## 2. Read the references that apply

rustc, clippy, and rustfmt review first (`M-STATIC-VERIFICATION`): `cargo fmt --check` and `rtk cargo clippy -p <crate>
--all-targets -- -D warnings`. Grep std at `$(rustc --print sysroot)/lib/rustlib/src/rust/library/std` (`rustup
component add rust-src`) before a local helper; never invent a std API. Read only the
[Book](https://doc.rust-lang.org/book/) and [Rust by Example](https://doc.rust-lang.org/rust-by-example/) chapters the
file uses (Foundation [learn catalog](https://www.rust-lang.org/learn); Klabnik/Nichols). Cite the [API
Guidelines](https://rust-lang.github.io/api-guidelines/checklist.html) (C-CONV, C-GETTER, C-CTOR) or Microsoft's
[Pragmatic Rust Guidelines](https://microsoft.github.io/rust-guidelines/) (`M-UPSTREAM-GUIDELINES`, `M-REGULAR-FN`) only
for the finding that rests on them. A public item with no caller is unused.

## 3. Compare similar patterns

Identify the file's public types, error enum, traits, Axum extractors, store module, or CLI subcommand. Search the
workspace for the same names, the `thiserror`/`anyhow` split, extractor order, and test layout. Cite at least two
siblings as `path:line`. A one-off that duplicates a helper two crates over is a finding; a one-off that is the helper
is not.

## 4. Answer every review question with evidence

Each answer needs `path:line`. "Seems fine" is not an answer.

- **Is this actually needed?** Delete the unused path and its seam, rather than documenting why it remains.
- **Does it describe only the present?** Remove chronology. Keep only the why behind a live invariant.
- **Is it tested?** Name the covering test, or the missing behavior. A compile is not a test.
- **Is it documented?** Module docs and `docs/` describe today. Do not doc a private helper whose name is the contract.
- **If applicable, is it featured in our Presentations or Workshops?** Search `server/content/workshops/navigator/`
  (`RUST_IN_PEACE.md` is the presentations catalog). A cited code slide is the source: update it with the file. Leave
  spoken workshop words alone; see [`authoring-slides`](../authoring-slides/SKILL.md).

No `unwrap` / `expect` / `panic!` outside `main()` and tests, no `unsafe`, Axum body extractors last.

## 5. Write the report, then take one action

Write one report at `/tmp/navigator-random-refactor/<file-stem>-<shortsha>.md`, including a zero-finding result. Keep
citations. Do not copy client data, legal files, real contact details, production identifiers, or Linear titles. Then
take exactly one action and keep the covering test green.

- **Zero findings, or an author or council decision:** stop. Hand the report back.
- **Clear, small, evidence-backed findings:** one finding, one seam. The seam may span the files a removal or a
  covering test needs (a removal removes the whole seam, per `rust`), never a second finding. Covering test first,
  present tense. Run `cargo fmt`, the focused test, `rtk cargo clippy -p <crate> --all-targets -- -D warnings`, and
  `cargo run -p cli --quiet -- project gate`. A deletion lands only with coverage re-verified; the floor never drops.
- **Teaching-surface drift only:** fix the doc, comment, or test. Do not rewrite workshop spoken words.

Open the change through [`create-pr`](../create-pr/SKILL.md): the report, minus anything private, is the PR body.
