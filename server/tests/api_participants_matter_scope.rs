#![allow(clippy::doc_markdown)]
//! Integration tests for `POST`/`PATCH`/`DELETE /app/api/projects/{id}/participants[/{role_id}]`
//! — the matter-scope check added alongside ENG-35's Clerk-visibility toggle.
//!
//! The write engine (`store::participation::{add,update,remove}_participant`) is
//! shared with the lawyer workbench's admin-only form; these tests focus on what
//! this REST door adds on top of the tier gate: a lawyer with no participation
//! row on the target matter is refused the same non-disclosing `404` an
//! unrelated caller would get (mirroring `POST .../close`), while a lawyer
//! already on the matter — or an Owner/Admin with the documented bypass — can
//! use the door to grant or revoke a Clerk's portal visibility by adding or
//! removing that Clerk's participation row.

#[path = "common/bearer.rs"]
mod bearer_common;
