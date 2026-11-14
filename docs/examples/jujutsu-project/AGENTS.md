# Working in this Navigator Project

Read `navigator.yaml` and stay in this Project checkout. Preserve the Project's approved agent instructions.

[Project repository contract](https://github.com/neon-law/navigator/blob/main/docs/project-repositories.md)

## Source and client records

- Declare Matter Pages in `navigator.yaml` under `pages:`. Keep reusable Drafts in `drafts/` and document Pointers in
  `blobs/`. Use Navigator's tools for Project records and approved storage for document bytes.
- Keep client data, legal files, credentials, and private notes out of commits, review requests, and screenshots.
  Check ignored paths first. Jujutsu snapshots working-copy changes automatically.
- Prepare email as a draft for lawyer review. The lawyer sends, replies, or forwards it personally.
- Review changes to client-facing lenses and document Pointer visibility before publishing.

## Navigator access

Sign in with `navigator site login` to use the local MCP bridge. It reads the saved session from `~/.navigator.json`.
Keep credentials out of tool arguments, chat, and repository files.

Use `navigator glossary list` and `navigator glossary show "<Term>"` for terminology.

## Jujutsu workflow

1. Inspect `jj status` and `jj diff`. Preserve unrelated work.
2. From a clean checkout, run `jj git fetch --remote origin` and `jj new main@origin` to begin one change.
3. Edit Project source. For a Matter Page change, run `hudson preview` and check the affected lenses.
4. Run `jj status` to snapshot the edits, then run `navigator project gate`. Review the full diff and describe the
   change with `jj describe -m "<summary>"`.
5. When authorized to publish, fetch and rebase onto current `main@origin`. Resolve conflicts and rerun the gate,
   verify the signature, and push only the reviewed change with `jj git push --remote origin --change @`.
6. Open a review request against `main` and verify the required `ci` check and merge result. Keep `main` protected.

Use the checkout's actual remote name if it differs from `origin`.

[Navigator how-to](https://github.com/neon-law/navigator/blob/main/docs/navigator-how-to.md)
