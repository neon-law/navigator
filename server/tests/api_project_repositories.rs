#![allow(clippy::doc_markdown)]
//! Integration tests for `GET /app/api/project-repositories` — the admin-only
//! reconciliation read.
//!
//! Two things are proved here that nothing else can prove:
//!
//! - **The report is an inventory, not a lens.** Every other matter read on
//!   this surface goes through `store::access::visible_projects`, which scopes
//!   to the caller's participation rows for every firm tier, Owner and Admin
//!   included. A reconciliation report built on that read would report a matter
//!   the caller does not participate in as a matter that does not exist. So the
//!   central test here holds one admin who participates in *nothing* and asserts
//!   that `GET /app/api/projects` returns zero for them while this door still
//!   reconciles every row.
//! - **The tier is enforced in the handler**, not only in policy.
//!   `portal::test_support::app_state` builds a router with the policy layer
//!   disabled, so a 403 observed here is the extractor's own `is_admin_tier`
//!   check. The Rego half of the same gate is proved separately, by the
//!   five-case matrix in `portal/policy/navigator_test.rego`. Both layers are
//!   deliberate: neither test would catch the other's regression.

#[path = "common/bearer.rs"]
mod bearer_common;
