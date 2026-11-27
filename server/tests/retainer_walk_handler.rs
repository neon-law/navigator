#![allow(clippy::doc_markdown)]
//! Integration tests for the stepwise retainer walker.
//!
//! Covers the full lifecycle:
//!   1. `POST /app/lawyer/retainers/new` creates Person + Project +
//!      role + Notation and redirects to `/step`.
//!   2. `GET /app/lawyer/notations/:id/step` renders the current
//!      question (read from the runtime + spec).
//!   3. `POST /app/lawyer/notations/:id/step` writes the Answer row and
//!      signals the runtime (the runtime — InMemoryRuntime in tests,
//!      the workflows-service worker in production — owns
//!      `notation_events`).
//!   4. The final POST hits END, drives the workflow, and renders
//!      the result page with the rendered retainer.

#[path = "common/intake_stack.rs"]
mod intake_stack;
