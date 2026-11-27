#![allow(clippy::doc_markdown)]
//! Integration tests for `POST /app/api/projects/{id}/conversation/messages` —
//! the client-writable REST door that posts a message to a matter conversation.
//!
//! The write engine (`conversation::post_conversation_message`) is shared with
//! the portal message control, so this focuses on what the REST adapter adds:
//! it is client-writable (both lenses post), the either-lens gate collapses a
//! non-participant to 404, an anonymous caller is 401, an empty body is 400, and
//! a participant's message actually lands with the tier-derived direction.

#[path = "common/bearer.rs"]
mod bearer_common;
