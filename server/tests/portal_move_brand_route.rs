//! Integration tests for the Owner-only move-brand door:
//! `POST /app/owner/firms/move-brand` — the portal route over
//! `store::firms::move_brand` (PR #206).
//!
//! These prove at the HTTP boundary what the store's own tests cannot:
//!
//! - **The Owner's move lands and reports the new attribution.** A `303` back
//!   to the control with `?moved=neon&firm=<target name>`, and the brand
//!   actually wears the target Firm afterwards.
//! - **Only the Owner gets through.** An Admin session receives the
//!   committed `404` the handler returns (Rego admits the session, the
//!   handler denies the tier — defense in depth), a request with no session
//!   at all never reaches the handler: the policy layer bounces it with a
//!   `303` to `/auth/login?return_to=…` ("click protected link → land at the
//!   `IdP`"), and the brand is untouched either way.
//! - **A refusal redirects back with the rule and both pickers echoed**, so
//!   nothing is retyped, and the brand is untouched.

#[path = "common/bearer.rs"]
mod bearer_common;
