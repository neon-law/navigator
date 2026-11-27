#![allow(clippy::doc_markdown)]
//! Integration tests for the matter-invoice door: `POST
//! /app/api/projects/{id}/invoices`, the command that raises one invoice
//! across the billing provider seam from the matter's unbilled work.
//!
//! Like the task doors (`api_project_tasks.rs`), the store under test is
//! PostgreSQL-native, so these tests run on the PostgreSQL lane
//! (`NAVIGATOR_TEST_POSTGRES_PORT`): each test process builds one
//! canonical-schema database, points the app's shared `PostgresDb::from_env()`
//! pool at it, and drives the real handlers through the full router —
//! embedded Rego policy included. Without the lane they fail.
//!
//! The provider is the billing crate's stub, held by the fixture so the
//! assertions can count actual `create_invoice` calls — the idempotency
//! contract is about how many times the seam is crossed, not only what the
//! mirror shows. A dedicated failing provider covers the refusal path.

#[path = "common/bearer.rs"]
mod bearer_common;

// A refused create preserves one reserved batch without an invoice; a
// retry must neither release its work nor compose a different intent.
