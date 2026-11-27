#![allow(clippy::doc_markdown)]
//! Integration tests for the client document-deletion REST doors:
//! `POST /app/api/expunge-requests/{id}/authorize` (admin) and `.../deny`
//! (lawyer/admin).
//!
//! The write engines (`expunge_request_route::authorize_expunge_request` and
//! `deny_expunge_request`) are shared with the lawyer queue forms, so this
//! focuses on what the REST adapters add: authorize is admin-only (a lawyer is
//! 403), deny is lawyer-tier (a client is 403), anonymous is 401, an unknown or
//! already-resolved request is 404/409, and the live authorize actually scrubs.

#[path = "common/bearer.rs"]
mod bearer_common;

// The test binary shares one process-wide Git root. Serializing its Git-backed
// document setup avoids instrumented macOS child processes racing inherited
// file descriptors while one test rewrites another matter's history.
