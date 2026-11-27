#![allow(clippy::doc_markdown)]
//! End-to-end tests for the browser-flow OAuth2 routes.
//!
//! These drive the real `server::neon_router()` via
//! `tower::ServiceExt::oneshot` and substitute a `wiremock`-backed
//! IdP for the upstream provider. No socket on the app side, no
//! browser anywhere.

#[path = "common/seed_person.rs"]
mod seed_person;
