#![allow(clippy::doc_markdown)]
//! Integration tests for the remaining #866 read doors:
//! `GET /app/api/projects/{id}/documents`, `/app/api/projects/{id}/conversation`,
//! `/app/api/expunge-requests`, `/app/api/notations/{id}/review-documents`.
//!
//! Focus: the client-lens filtering on the client-readable doors (documents and
//! conversation hide internal work product, #782), the lawyer-tier gate on the
//! firm-tool doors (expunge queue, review drafts), and the matter-scope 404.

#[path = "common/bearer.rs"]
mod bearer_common;
