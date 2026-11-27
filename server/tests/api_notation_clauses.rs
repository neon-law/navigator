#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/notations/{id}/clauses` — append a custom
//! clause to a notation's document.
//!
//! The store command (`store::notation_clauses::append`) is the one the lawyer
//! clause form already drives, so these tests focus on the REST adapter: the
//! tier gate (LawyerSession → 401/403), the matter-scope gate (a non-participant
//! lawyer gets a bare 404, admin bypasses), validation (blank body → 400), and a
//! live 201 that returns the appended clause id.

#[path = "common/bearer.rs"]
mod bearer_common;
