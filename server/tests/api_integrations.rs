#![allow(clippy::doc_markdown)]
//! Integration tests for the `/app/api/integrations/*` doors.
//!
//! Three things are proved here that nothing else proves:
//!
//! - **The tier is enforced in the handler**, not only in policy.
//!   `portal::test_support::app_state` builds a router with the policy layer
//!   disabled, so a 403 observed here is the handler's own `is_admin_tier`
//!   check. The Rego half of the same gate lives in
//!   `portal/policy/navigator_test.rego`; neither test catches the other's
//!   regression.
//! - **`--all` is the caller's lens, not the whole deployment.** The sweep
//!   runs `store::access::visible_projects`, so an admin who participates in
//!   nothing sweeps nothing. A door that read every row instead would let an
//!   `--all` run touch a matter the read surface hides.
//! - **A deployment with no runtime KMS key refuses rather than falling
//!   back.** That is the whole point of the Firm-secret boundary: a Project
//!   must never reach a provider credential its Firm did not write, so
//!   "unconfigured" is an outcome and not a stub that pretends to succeed.

#[path = "common/bearer.rs"]
mod bearer_common;

// ---- ENG-807: recorded-identity validation, recovery, and concurrency ----
