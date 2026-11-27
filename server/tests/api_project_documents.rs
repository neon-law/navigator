#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/projects/{id}/documents` — the REST door
//! that files a document into a matter.
//!
//! The write engine (`matter_documents::record_document`) is shared with the
//! lawyer upload control, so this focuses on what the REST adapter adds: it takes
//! the bytes base64-encoded (not multipart), lawyer-tier only (client 403, anon
//! 401), the matter-scope gate (a non-participant lawyer is 404), a missing `kind`
//! is 400, undecodable base64 is 400, a `kind` outside the asset lane is 400 (not
//! a 500), and a filed document actually lands.

#[path = "common/bearer.rs"]
mod bearer_common;
