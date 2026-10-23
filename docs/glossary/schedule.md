---
title: "Schedule"
description: "A Schedule is a questionnaire answer made of repeating rows whose cells are text, a number, or a date."
---

A [Question Type](question-type.md) (`schedule`) whose answer is a list of rows. Each column is `text`, `number`, or
`date`. A Schedule does not ground to a table, and it is not a document body: the cells live in the answer JSON. Authors
declare `columns:` on the `schedule__<role>` [State](state.md).
