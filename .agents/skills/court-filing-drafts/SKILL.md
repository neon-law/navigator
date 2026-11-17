---
name: court-filing-drafts
description: >-
  Mark every draft of a court filing counsel will sign, and clear the marks to finalize it. Use when drafting,
  exporting, or finalizing a stipulation, motion, declaration, answer, or proposed order.
---

# Court filing drafts

Before naming a record or UI concept, follow **Find the term first** in `AGENTS.md`.

Every draft of a court filing counsel will sign carries the marks below, so it reads as a draft at a glance. They come
off only at finalizing.

## Draft marks

- **Counsel signature.** Each counsel signature line reads an italic `/s/ DRAFT`, with `DRAFT` in capitals and
  highlighted yellow. It sits directly on the signature rule, drawn as the name paragraph's bottom border, never on an
  underscore line.
- **Empty dates.** Every empty date is highlighted yellow, such as `DATED this ___ day of ________, 20__`.
- **Other open items.** A tentative date or a fact to confirm is highlighted yellow.
- **The judge's lines.** On a proposed order, the judge's signature and date lines stay plain and blank.

## In a notation

Write each counsel signature as `<!-- counsel-signature -->` in its own paragraph. Collect a date through the
questionnaire, as a `{{placeholder}}`. Write the judge's lines as plain text with a rule, such as `DISTRICT COURT JUDGE:
______________`.

```bash
navigator notation word --preview drafts/<code>.md --out "documents/Work Product/drafts/<code>.docx"
navigator notation pdf --preview drafts/<code>.md --out "documents/Work Product/drafts/<code>.pdf"
```

A preview draws the counsel signature as italic `/s/ DRAFT` on its rule, with `DRAFT` highlighted, and highlights each
unanswered placeholder and open drafting marker. The judge's plain lines stay untouched.

A final render, without `--preview`, refuses any unanswered placeholder or drafting marker. It signs each counsel
signature as italic `/s/ <name>` on the same rule, from `matter.counsel.name` in `navigator.yaml`.

## Finalizing

1. Accept or reject every tracked change, and remove every highlight.
2. Replace each `/s/ DRAFT` with an italic `/s/ Name`. Sign another attorney's name only with that attorney's written
   authorization.
3. Fill counsel's dated lines. Leave the judge's date blank.
4. Keep `[PROPOSED]` in a proposed order's title.
5. Remove every placeholder.
