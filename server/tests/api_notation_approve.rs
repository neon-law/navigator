#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/notations/{id}/approval` — the REST door
//! that approves a notation parked at `lawyer_review` (fires `approved`, parks
//! at the generate-PDF step).
//!
//! The command core (`render_and_park`) is shared with the lawyer review screen
//! (covered in `retainer_walk_handler.rs`). These tests cover what the REST
//! adapter adds: the tier gate (LawyerSession → 401/403), the matter-scope gate
//! (a lawyer who does not participate in the notation's matter gets a bare 404,
//! admin bypasses), and a live 200 advancing a `lawyer_review` notation. The
//! fixture uses the shared `advance_to_lawyer_review` driver to reach the gate.

#[path = "common/bearer.rs"]
mod bearer_common;
