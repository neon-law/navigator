#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/notations/{id}/answers` — the REST door
//! that answers a notation's current questionnaire step.
//!
//! The write engine (`workflows::answer_step_with_reference`) is shared with
//! the lawyer retainer walk, so these tests focus on what the REST adapter
//! adds: the tier gate (LawyerSession → 401/403), the matter-scope gate (a
//! lawyer who does not participate in the notation's matter gets a bare 404,
//! admin bypasses), the out-of-order-step contract (409 question_mismatch),
//! and a live 200 proving the step JSON.

#[path = "common/bearer.rs"]
mod bearer_common;

// Every case configures the process-wide repo root and commits through the
// same filesystem-backed test forge. Keep that environment/filesystem seam
// single-owner while each request is exercised.
