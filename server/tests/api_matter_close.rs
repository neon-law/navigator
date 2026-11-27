#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/projects/{id}/close` — the REST door
//! that opens a matter's firm-signed closing-letter notation.
//!
//! The write engine (`retainer_walk::open_closing_notation`) is shared with the
//! lawyer close control, so these tests focus on what the REST adapter adds: the
//! tier gate (LawyerSession → 401/403), the matter-scope gate (a lawyer who does
//! not participate is a bare 404, admin bypasses), the no-client refusal (409),
//! and a live 201 proving the closing notation is opened on the matter.

#[path = "common/bearer.rs"]
mod bearer_common;
