---
name: word-editing
description: >-
  Open a notation in Microsoft Word and carry the lawyer's edits back to its template while preserving placeholders.
---

# Word editing

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

The template under `drafts/` (or `templates/`) stays the source of truth. Word is where the lawyer edits; the template
is where the edit lands. Never commit a `.docx`.

## Open it in Word

```bash
hudson notation word --preview drafts/<code>.md --out "documents/Work Product/drafts/<code>.docx"
open -a "Microsoft Word" "documents/Work Product/drafts/<code>.docx"
```

The preview keeps every `{{placeholder}}` verbatim and records the template hash and block map.

Tell the lawyer three things:

- edit freely, but leave each `{{placeholder}}` as it is; a fact belongs in the questionnaire, not the template;
- accept or reject every tracked change before saving;
- save in place, under the same name, as Word Document (.docx).

## Update the notation

When the lawyer says the file is saved, preview the block-level changes:

```bash
hudson notation import drafts/<code>.md "documents/Work Product/drafts/<code>.docx" --dry-run
```

If the command reports unresolved revisions or comments, have the lawyer resolve them in Word. If the template changed
after export, compare the conflicting blocks with the lawyer's edits before exporting a fresh draft. If an edit changes
a placeholder or Clause include, review the proposed wording and keep the template token intact.

Review the diff before applying it. Run the same command without `--dry-run` to apply the plan. Import checks the edited
template against the blocking project rules before writing. Then run `navigator project gate` and render a fresh PDF
with `hudson notation pdf --preview` for the lawyer to check. Report the edits applied and any held items.
