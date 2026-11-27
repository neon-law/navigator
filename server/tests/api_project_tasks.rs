#![allow(clippy::doc_markdown)]
//! Integration tests for the matter-task doors: the Tasks tab's write surface
//! beside the project-costs API — create, assign, and transition, plus the
//! list read.
//!
//! The task store is PostgreSQL-native, so these tests run on the PostgreSQL
//! lane (`NAVIGATOR_TEST_POSTGRES_PORT`, the store crate's own fixture
//! contract): each test process builds one canonical-schema database, points
//! the app's shared `PostgresDb::from_env()` pool at it, and drives the real
//! handlers through the full router — embedded Rego policy included. Without
//! the lane they fail.

#[path = "common/bearer.rs"]
mod bearer_common;
