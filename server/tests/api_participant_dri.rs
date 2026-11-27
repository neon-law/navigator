#![allow(clippy::doc_markdown)]
//! Integration tests for `PUT`/`DELETE /app/api/projects/{id}/participants/{role_id}/dri`
//! — the REST doors that designate or clear a matter participant's DRI marker.
//!
//! The write engine (`store::participation::update_participant`) is shared with
//! the lawyer workbench control, so these tests focus on what the REST adapters
//! add: the tier gate (LawyerSession → 401/403), the matter-scope check (a
//! `role_id` from another matter is a bare 404), the actor gate the command
//! enforces (a lawyer who does not already hold the marker is 403), the
//! emptiness rule (clearing the matter's last lawyer DRI is 422), and the live
//! 204s proving the marker actually moves.

#[path = "common/bearer.rs"]
mod bearer_common;
