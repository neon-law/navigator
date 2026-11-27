#![allow(clippy::doc_markdown)]
//! Integration tests for editing, removing, and reordering a notation clause:
//! `PATCH`/`DELETE /app/api/notations/{id}/clauses/{clause_id}` and
//! `POST .../move`.
//!
//! The store commands (`update_body`/`delete`/`move_clause`) are the ones the
//! lawyer clause form already drives. These tests cover the REST adapter: the
//! tier gate (401/403), the matter-scope gate + the clause-belongs-to-notation
//! guard (both → 404), blank-body 400, and the live 200/204 outcomes.

#[path = "common/bearer.rs"]
mod bearer_common;
