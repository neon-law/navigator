---
name: word-editing
description: >
  Let the lawyer edit a notation in local Microsoft Word, then carry those edits back into its template. Trigger when
  the lawyer asks to open a draft, memo, letter, or agreement "in Word", or, after editing one there, asks to "update
  the notation". Kept identical, byte for byte, in every Project repository's `.agents/skills/`; this canonical copy
  lives in Navigator's own `.agents/skills/`.
---

# Word editing

Before naming a table, field, state, or UI concept, follow the **Find the term first** section in `AGENTS.md`.

The template under `drafts/` (or `templates/`) stays the source of truth. Word is where the lawyer edits; the template
is where the edit lands. Never commit a `.docx`.

## Open it in Word

```bash
navigator notation word --preview drafts/<code>.md --out "documents/Work Product/drafts/<code>.docx"
cp "documents/Work Product/drafts/<code>.docx" "documents/Work Product/drafts/<code>.baseline.docx"
open -a "Microsoft Word" "documents/Work Product/drafts/<code>.docx"
```

The baseline copy is what the lawyer's edits are measured against. Make it in the same step as the export, before Word
opens the file. `--preview` keeps every `{{placeholder}}` verbatim, which is what lets an edit map back to the template.

Tell the lawyer three things:

- edit freely, but leave each `{{placeholder}}` as it is; a fact belongs in the questionnaire, not the template;
- accept or reject every tracked change before saving;
- save in place, under the same name, as Word Document (.docx).

## Update the notation

When the lawyer says the file is saved:

1. **Refuse unresolved tracked changes.** Search the edited file's `word/document.xml` for a `w:ins` or `w:del` element.
   If one is present, stop and ask the lawyer to accept or reject them.
2. **Diff the text.** Convert the baseline and the edited copy with `textutil -convert txt -stdout` and diff them. Each
   hunk is one edit to carry back.
3. **Read the comments.** List `word/comments.xml`, if present. A comment is an instruction or a question: apply it or
   answer it, and never drop one silently.
4. **Apply each hunk to its source block** in the template. Change only the words the lawyer changed. Keep the
   template's Markdown: bold, italics, headings, list markers, and table rows. Leave the frontmatter alone.
5. **Guard the placeholders.** If a hunk edits, splits, or deletes a `{{placeholder}}` or a `{{> clause}}` include,
   or replaces a placeholder with a filled value, do not apply it. Show it to the lawyer and ask.
6. **Gate and re-render.** Refill changed prose to 120 columns, run `navigator project gate`, and render the PDF with
   `navigator notation pdf --preview` for the lawyer to check.
7. **Report.** List every hunk applied, every hunk held, and every comment, with its answer.
8. **Reset the baseline.** Export again and copy a new baseline, so the next round diffs against what the template now
   says.

## Limits

This diff is done by hand. A deterministic import verb, which checks the template's hash and maps each Word paragraph to
its block, is tracked in LAW-229. When it ships, use it in steps 1 through 5.
