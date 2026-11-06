---
title: "Questionnaire route"
description: >-
  The edge a questionnaire walk follows after an answer: the choice's own named route, or the underscore fallback.
---

The edge a questionnaire walk follows out of a state once an answer is recorded. A linear state has one `_` edge — the
same next question for every answer. A branch state (a single-choice question) may name one edge per declared `choices:`
value: the answer takes its own route, and a `_` fallback catches any answer that names no route. Every declared choice
must have its own route or share the fallback, and every route must reach `END`; the workflow gate refuses a template
that strands an answer or leaves a question unreachable — at compile time, never mid-walk (N118).

A body read must be collected on every route that can render it. A route-specific answer belongs inside a matching
conditional; nested `if`, `else if`, and `else` branches constrain the routes that render their reads (N115/N120).
