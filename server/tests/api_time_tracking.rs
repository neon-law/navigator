#![allow(clippy::doc_markdown)]
//! Integration tests for the time-tracking doors: the Billing tab's time
//! surface beside the project-tasks API — manual entries, invoice marking and
//! write-offs, the durable Timer (start, stop, discard), and the fee
//! structures the entries are priced against.
//!
//! Like the task doors, these run on the PostgreSQL lane
//! (`NAVIGATOR_TEST_POSTGRES_PORT`): each test process builds one
//! canonical-schema database, points the app's shared `PostgresDb::from_env()`
//! pool at it, and drives the real handlers through the full router — embedded
//! Rego policy included. Without the lane they fail.

#[path = "common/bearer.rs"]
mod bearer_common;
