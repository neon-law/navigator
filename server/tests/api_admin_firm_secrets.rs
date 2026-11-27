//! Integration tests for the Firm integration-secret settings door
//! (ENG-491): `POST /app/admin/firms/{id}/secrets` and
//! `POST /app/admin/firms/{id}/secrets/revoke`.
//!
//! These prove what nothing else proves at the HTTP boundary:
//!
//! - **Only the Firm's own Admin DRI writes.** A non-DRI Admin and Owner both
//!   get the store's `NotAuthorized` refusal (mapped to a plain redirect, not
//!   a disclosure), matching `store::firm_secrets::put`'s own rule.
//! - **A write never echoes the submitted value.** Every redirect target
//!   (success and failure) is asserted to omit the exact string submitted, in
//!   the response body *and* the `Location` header.
//! - **Cross-Firm denial.** An Admin DRI of Firm A cannot write or read Firm
//!   B's secrets through this door.
//! - **Rotation and revoke round-trip through the same page** the create form
//!   posts to, and the Firm show page renders the resulting metadata (not a
//!   plaintext value) once the caller can view it.

#[path = "common/bearer.rs"]
mod bearer_common;
