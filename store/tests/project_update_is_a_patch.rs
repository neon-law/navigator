//! `update_project` is always a patch: an absent field is never written.
//!
//! The rule these tests pin is the one that keeps a partial update from
//! destroying data the caller never mentioned. A client that reads a matter,
//! edits one field, and sends its own narrow body must not be able to erase the
//! columns it does not know about — so absence means *leave alone*, and
//! clearing is always an explicit empty string.
//!
//! `null` and absent are deliberately the same thing. `serde` cannot tell them
//! apart on a plain `Option`, and collapsing them toward "leave alone" is the
//! safe direction: the other choice fails by wiping a column silently.

use store::projects::UpdateProjectCommand;

/// `status` is not a column this patch may touch — moving a matter through
/// its lifecycle belongs to `POST /app/api/projects/{id}/lifecycle`
/// (`transition_project`). A body naming it must fail to deserialize rather
/// than being silently dropped, so `{"status": "zzz-not-a-status"}` can never
/// apply as a no-op `200` a caller could mistake for success.
#[test]
fn a_status_field_fails_to_deserialize_rather_than_being_dropped() {
    let err = serde_json::from_value::<UpdateProjectCommand>(serde_json::json!({
        "status": "zzz-not-a-status",
    }))
    .expect_err("status is not a field on this patch");
    assert!(
        err.to_string().contains("status"),
        "the error should name the offending field: {err}"
    );
}

/// `closed_at` is derived from the transition inside `transition_project`
/// and must never be independently settable — not even to a value that
/// looks legitimate. Same failure mode as `status` above.
#[test]
fn a_closed_at_field_fails_to_deserialize_rather_than_being_dropped() {
    let err = serde_json::from_value::<UpdateProjectCommand>(serde_json::json!({
        "closed_at": "2020-01-01T00:00:00Z",
    }))
    .expect_err("closed_at is not a field on this patch");
    assert!(
        err.to_string().contains("closed_at"),
        "the error should name the offending field: {err}"
    );
}

/// Any other unrecognized field is rejected the same way — the point is a
/// no-op write can never pass for a successful one, not that these two
/// fields specifically are blocklisted.
#[test]
fn an_unknown_field_fails_to_deserialize_rather_than_being_dropped() {
    let err = serde_json::from_value::<UpdateProjectCommand>(serde_json::json!({
        "made_up_field": "anything",
    }))
    .expect_err("an unrecognized field must not silently deserialize");
    assert!(
        err.to_string().contains("made_up_field"),
        "the error should name the offending field: {err}"
    );
}
