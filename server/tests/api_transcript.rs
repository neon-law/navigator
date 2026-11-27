#![allow(clippy::doc_markdown)]
//! Integration tests for the transcript REST door:
//! `POST /app/api/notations/{id}/transcript` (batch coverage).
//!
//! The write engine (`retainer_walk::record_transcript_coverage`) is shared
//! with the lawyer form, so this focuses on what the REST adapter adds: it
//! takes the transcript as JSON text (not multipart), lawyer-tier only
//! (client 403, anon 401), matter-scope (out-of-scope 404), and the live
//! coverage run.

#[path = "common/bearer.rs"]
mod bearer_common;

// ---------- batch transcript coverage ----------
