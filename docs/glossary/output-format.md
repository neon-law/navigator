---
title: "Output Format"
description: "An Output Format is the frame a notation renders in, set by its output key or its Notation Kind."
---

The frame wrapped around a rendered notation. `navigator notation pdf` resolves it in one order and there is no
`--format` flag: the template's own `output:` value, else the default its [Notation Kind](notation-kind.md) derives,
else plain. Rule `N109` accepts exactly the values below, so a misspelling fails validation instead of quietly falling
back to plain.

- *(omitted)* / plain — Unadorned page, one-inch margins, no firm marks. Never declared: omitting `output:` on a kind
  whose default is plain is how a template selects it. Default for: `will`, `trust`, `directive`.
- `letter` — The firm letterhead (logo, firm name, contact line, rule) over airy geometry, with the date,
  recipient, `Re:`, salutation, closing, enclosures, and `cc:` blocks. Default for: `letter`, `onboarding`,
  `offboarding`, `memo`.
- `client_letter` — The same letter geometry and blocks with **no letterhead**, for a letter sent in the client's own
  name. Declare it on a `kind: letter` template. Default for: nothing; always declared.
- `contract` — An executed instrument: no letterhead, denser leading, visible heading hierarchy, unbreakable signature
  tables, and no numbering of its own. `agreement` is its retired spelling and still parses. Default for: `agreement`.
- `pleading` — Court paper with no letterhead, calibrated by the template's `jurisdiction:` (`NV`, `CA`, `US`); an
  uncalibrated jurisdiction is refused. Default for: `pleading`.
- `form` — Not typesetting: an AcroForm fill of an official government form. Requires `form:` and `origin_url:`, and
  those keys are allowed on no other output. Default for: `filing`.
- `resume` — Dense geometry for one or two pages, no letterhead, tight list spacing, and everything above the first `##`
  section (the `#` name and its contact line) centered as the header. Default for: `resume`.

A letter always carries the firm letterhead unless it declares `output: client_letter`; the other kinds render without
one.

Run `navigator notation kinds` for both tables, and see `navigator notation pdf --help` for the frame each kind gets.

- Source: [`pdf::OutputFormat`](../../pdf/src/format.rs) · Validator:
  [`N109`](../../rules/src/f109.rs)
