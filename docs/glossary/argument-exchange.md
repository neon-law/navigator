---
title: "Argument Exchange"
description: >-
  An Argument Exchange is an ordered point, expected response, and reply, each with supporting authority, on a lawyer's
  hearing console.
---

An Argument Exchange gives each side of an argument room for several lines. The lawyer authors exchanges in a
`trial_prep` Notation. Each exchange has a title and three turns: `point`, `response`, and `reply`. Every turn carries
`text` and `authority`. Authority text identifies the support for that turn; it does not create an
[Authority](authority.md) record or verify the cited source.

Declare the optional `argument_exchange` [Section](section.md) on the lawyer lens of a `hearing_console` Matter Page.
The page's `draft:` selects the Notation. Only an activated Notation whose pinned body passes digest verification
supplies exchanges. Client and clerk lenses receive none. `stage_list` remains the hearing console's required section.

Author exactly one fenced `argument_exchange` block containing a YAML sequence. Sequence order is display order. Use
YAML block scalars for multiline turns; each physical source line can stay within the gate's length limit.

```yaml
- title: Synthetic argument
  point:
    text: |
      First part of our point.
      Further explanation on another line.
    authority: Synthetic Rule A
  response:
    text: |
      The expected response.
      Its supporting explanation.
    authority: Synthetic Rule B
  reply:
    text: Our reply.
    authority: Synthetic Rule C
```

All fields are required and must contain nonblank text. Unknown fields, malformed YAML, an unclosed block, or more than
one exchange block refuse the entire exchange list. A refused block shows an unavailable message; a missing or empty
block shows the section's empty state. Text and authority display as escaped plain text with line breaks preserved.
Embedded HTML and Markdown do not execute or create links.
