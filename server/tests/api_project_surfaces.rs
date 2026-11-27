#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/project-surfaces/{id}` — the admin-only
//! retry that creates or adopts a Project's Drive ingest folder and source
//! repository.
//!
//! Two things are proved here that the store tests cannot:
//!
//! - **The tier is enforced in the handler**, not only in policy.
//!   `portal::test_support::app_state` builds a router with the policy layer
//!   disabled, so a 403 observed here is the extractor's own `is_admin_tier`
//!   check.
//! - **An unassigned admin still reaches a matter they do not participate
//!   in.** Every other matter write on this surface is participation-scoped;
//!   this door is the deployment retry, so it keys off the id alone.

#[path = "common/bearer.rs"]
mod bearer_common;
