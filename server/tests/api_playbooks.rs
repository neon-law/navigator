#![allow(clippy::doc_markdown)]
//! Integration tests for the contract-review playbook REST doors:
//! `POST /app/api/playbooks` and `PUT /app/api/playbooks/{id}`.
//!
//! The write engines (`store::playbooks::create` / `update_positions`) are
//! shared with the lawyer playbook forms, so this focuses on what the REST
//! adapters add: they take structured positions (not a pipe-delimited textarea),
//! lawyer-tier only (client 403, anon 401), a blank name or empty set is 400, a
//! duplicate name is 409, an unknown playbook is 404, and a create/update lands.

#[path = "common/bearer.rs"]
mod bearer_common;
