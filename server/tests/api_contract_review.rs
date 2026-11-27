#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/projects/{id}/contract-review` — a
//! multipart, client-writable door that uploads a contract for deviation
//! review.
//!
//! The command (`drive_contract_review`) is shared with the lawyer/portal form.
//! These tests cover the REST adapter: the tier/participation gate (401 anon,
//! 404 non-participant), the no-playbook precondition (422), and a live 201 —
//! against the deterministic `StubContractReviewer`.

#[path = "common/bearer.rs"]
mod bearer_common;
