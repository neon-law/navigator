#![allow(clippy::doc_markdown, clippy::too_many_lines)]
//! Integration tests for the contract-review workbench REST doors:
//! `POST /app/api/contract-reviews/{id}/{findings/{idx},summary,approve,reject}`.
//!
//! The write engines (`admin_contract_reviews::{save_review_finding,
//! save_review_summary, approve_review, reject_review}`) are shared with the
//! lawyer review surface, so this focuses on what the REST adapters add: the tier
//! gate (client 403, anon 401), the matter-scope gate (a non-participant lawyer
//! is a bare 404, admin bypasses), the approval guard (422 before every finding
//! is acted on), and the live accept → summary → approve / reject flows.

#[path = "common/bearer.rs"]
mod bearer_common;
