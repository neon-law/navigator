#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/notations/{id}/signature` — the REST door
//! that dispatches a notation's rendered document for signature.
//!
//! The command core (`send_notation_for_signature`) is shared with the lawyer review
//! screen. These tests cover what the REST adapter adds: the tier gate
//! (LawyerSession → 401/403), the matter-scope gate (a non-participant lawyer
//! gets a bare 404, admin bypasses), the readiness gate (send before the PDF
//! is rendered → 409 document_not_ready), and a live 200 after an approval
//! rendered the PDF (a `DispatchingRuntime` runs the generate-PDF worker; a
//! `FakeSignatureProvider` records the dispatched envelope).

#[path = "common/bearer.rs"]
mod bearer_common;
