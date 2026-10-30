//! Communication doors (LAW-140).
//!
//! The read is the firm view of a Project's conversation log: every row,
//! internal notes and logged calls included, at Lawyer tier and scoped to a
//! participant. A client reads the conversation through
//! `GET .../conversation`, which excludes internal notes in the query. The
//! write logs a call as an internal-note Communication with its structure,
//! or an email with its source, counterparty role, and optional call confirmation or receipt relation.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use store::persons::Role;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateCommunicationRequest {
    /// `call`, `email_inbound`, or `email_outbound`.
    channel: String,
    /// `inbound` or `outbound`.
    direction: String,
    counterparty: String,
    /// The counterparty's `opposing_counsel`, `client`, `court`, or `other` role.
    role: Option<String>,
    /// RFC 3339: when the call or email happened.
    at: String,
    /// Opaque client-supplied idempotency key, such as a provider Message-ID.
    /// This value is not proof of provider identity.
    source_ref: Option<String>,
    duration_minutes: Option<i64>,
    body: String,
    subject: Option<String>,
    /// `none` or `all_party_consent`.
    recorded: Option<String>,
    meet_and_confer: Option<String>,
    #[serde(default)]
    agreements: Vec<String>,
    /// The call whose agreements this email confirms.
    confirms: Option<Uuid>,
    /// Outbound email whose receipt this inbound email acknowledges.
    acknowledges: Option<Uuid>,
    #[serde(default)]
    client_visible: bool,
}

async fn require_participant(
    state: &ApiState,
    person_id: Option<Uuid>,
    role: Role,
    project_id: Uuid,
) -> Result<(), ApiError> {
    let allowed = store::access::can_see_project(&state.surreal, person_id, role, project_id)
        .await
        .map_err(ApiError::Db)?;
    if allowed {
        Ok(())
    } else {
        Err(ApiError::NotFound)
    }
}

fn map_communication(error: store::communications::CommunicationError) -> ApiError {
    match error {
        store::communications::CommunicationError::Invalid(message) => {
            ApiError::CommunicationInvalid(message)
        }
        store::communications::CommunicationError::NotFound => ApiError::NotFound,
        store::communications::CommunicationError::Db(error) => ApiError::Db(error.to_string()),
    }
}

fn validate_source_ref<'a>(
    channel: &str,
    source_ref: Option<&'a str>,
) -> Result<Option<&'a str>, ApiError> {
    match channel.trim() {
        store::communications::channel::CALL if source_ref.is_some() => {
            Err(ApiError::CommunicationInvalid(
                "source_ref is only accepted for email Communications".into(),
            ))
        }
        store::communications::channel::EMAIL_INBOUND
        | store::communications::channel::EMAIL_OUTBOUND => {
            let source_ref = source_ref.map(str::trim);
            if source_ref.is_some_and(|value| value.is_empty() || value.len() > 512) {
                return Err(ApiError::CommunicationInvalid(
                    "source_ref must contain 1 to 512 bytes".into(),
                ));
            }
            Ok(source_ref)
        }
        _ => Ok(None),
    }
}

/// The list door's optional filters (LAW-238 D3): `?direction=` narrows to
/// one direction (`inbound`, `outbound`, or `internal`), and
/// `?attachments=true` keeps only rows with a filed attachment. Both
/// compose with each other and with the CLI's `--channel`; unknown
/// direction values are a client error, not a silent empty list.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListCommunicationsFilter {
    /// Exact channel match — the filter the CLI's `--channel` has always
    /// applied, now applied server-side so all three compose in one query.
    channel: Option<String>,
    direction: Option<String>,
    attachments: Option<bool>,
}

/// One attachment on a Communication, as the list surfaces it (AC10): the
/// asset the bytes live under, its filename, and its kind.
#[derive(Debug, PartialEq, Eq, Serialize)]
pub(crate) struct AttachmentView {
    pub asset_id: Uuid,
    pub filename: Option<String>,
    pub kind: Option<String>,
}

/// One row of the list response: the Communication plus its attachments —
/// the row's own `asset_id`, and, for rows projected from the Inbox
/// (`source_ref = "inbox:{message-id}"`), the provenance-linked set filed
/// by the import (LAW-238 D1). A read-side join: no schema change.
#[derive(Debug, Serialize)]
pub(crate) struct CommunicationView {
    #[serde(flatten)]
    pub communication: store::communications::Communication,
    pub attachments: Vec<AttachmentView>,
}

/// `inbox:{message-id}` → the message id; `None` for any other `source_ref`.
fn inbox_message_id(source_ref: &str) -> Option<&str> {
    source_ref
        .strip_prefix("inbox:")
        .filter(|id| Uuid::parse_str(id).is_ok())
}

/// An unknown direction is the caller's mistake — a 400, not a silent empty
/// list. A row projected from the Inbox carries its message direction; a
/// call row's `direction` is its visibility (`internal` unless shared), so
/// `direction=internal` and `direction=outbound` partition the same log the
/// client lens already partitions.
fn validate_direction(direction: Option<&str>) -> Result<Option<&str>, ApiError> {
    let Some(direction) = direction.map(str::trim).filter(|d| !d.is_empty()) else {
        return Ok(None);
    };
    if matches!(
        direction,
        store::communications::direction::INBOUND
            | store::communications::direction::OUTBOUND
            | store::communications::direction::INTERNAL
    ) {
        Ok(Some(direction))
    } else {
        Err(ApiError::CommunicationInvalid(
            "direction must be inbound, outbound, or internal".into(),
        ))
    }
}

/// The attachment set one row lists: the row's own asset first, then the
/// provenance-linked assets the import filed (`mail/{message_id}/{index}`),
/// deduped by asset id — the first attachment occupies both slots (AC8).
fn attachment_views(
    primary: Option<store::assets::Asset>,
    filed: Vec<store::assets::Asset>,
) -> Vec<AttachmentView> {
    let mut attachments = Vec::new();
    let mut seen = Vec::new();
    for asset in primary.into_iter().chain(filed) {
        if seen.contains(&asset.id) {
            continue;
        }
        seen.push(asset.id);
        attachments.push(AttachmentView {
            asset_id: asset.id,
            filename: asset.filename.clone(),
            kind: asset.kind.clone(),
        });
    }
    attachments
}

/// `GET /app/api/projects/{id}/communications`
pub(crate) async fn list_communications(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Query(filter): Query<ListCommunicationsFilter>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let direction = validate_direction(filter.direction.as_deref())?;
    let attachments_only = filter.attachments.unwrap_or(false);
    let rows = store::communications::for_project(&state.surreal, id)
        .await
        .map_err(map_communication)?;
    let mut views = Vec::with_capacity(rows.len());
    for row in &rows {
        // The join, not the row shape, carries attachments: the row's own
        // asset slot plus, for Inbox-projected rows, every provenance-linked
        // asset the import filed (AC10).
        let primary = match row.asset_id {
            Some(asset_id) => store::assets::find_by_id(&state.surreal, asset_id)
                .await
                .map_err(ApiError::Asset)?,
            None => None,
        };
        let filed = match row.source_ref.as_deref().and_then(inbox_message_id) {
            Some(message_id) => {
                store::assets::find_by_source_message_id(&state.surreal, id, message_id)
                    .await
                    .map_err(ApiError::Asset)?
            }
            None => Vec::new(),
        };
        views.push(CommunicationView {
            communication: row.clone(),
            attachments: attachment_views(primary, filed),
        });
    }
    views.retain(|view| {
        filter
            .channel
            .as_deref()
            .is_none_or(|channel| view.communication.channel == channel.trim())
            && direction.is_none_or(|direction| view.communication.direction == direction)
            && (!attachments_only || !view.attachments.is_empty())
    });
    Ok(Json(views).into_response())
}

/// Logs an email Communication, then links any call it confirms.
async fn create_email(
    state: &ApiState,
    author_person_id: Option<Uuid>,
    project_id: Uuid,
    channel: &str,
    source_ref: Option<&str>,
    input: &CreateCommunicationRequest,
) -> Result<store::communications::Communication, ApiError> {
    let direction = match input.direction.trim() {
        "inbound" => store::communications::direction::INBOUND,
        "outbound" => store::communications::direction::OUTBOUND,
        _ => {
            return Err(ApiError::CommunicationInvalid(
                "direction must be inbound or outbound".into(),
            ))
        }
    };
    let at = chrono::DateTime::parse_from_rfc3339(input.at.trim())
        .map(|at| at.to_rfc3339())
        .map_err(|_| {
            ApiError::CommunicationInvalid(
                "at must be an RFC 3339 timestamp with an explicit offset".into(),
            )
        })?;
    if input.body.trim().is_empty() {
        return Err(ApiError::CommunicationInvalid("the body is empty".into()));
    }
    let ingested = store::communications::ingest_email(
        &state.surreal,
        &store::communications::IngestArgs {
            project_id,
            channel,
            direction,
            author_person_id,
            counterparty: Some(input.counterparty.trim()),
            subject: input.subject.as_deref(),
            body: input.body.trim(),
            source_ref,
            asset_id: None,
            occurred_at: &at,
        },
        store::communications::EmailDetails {
            counterparty_role: input.role.as_deref().map(str::trim),
            acknowledges: input.acknowledges,
        },
    )
    .await
    .map_err(map_communication)?;
    if let Some(call_id) = input.confirms {
        store::communications::confirm_agreements(
            &state.surreal,
            project_id,
            call_id,
            ingested.communication_id,
        )
        .await
        .map_err(map_communication)?;
    }
    store::communications::find(&state.surreal, project_id, ingested.communication_id)
        .await
        .map_err(map_communication)?
        .ok_or_else(|| ApiError::Db("the email could not be read back".into()))
}

/// `POST /app/api/projects/{id}/communications`
pub(crate) async fn create_communication(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<CreateCommunicationRequest>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let source_ref = validate_source_ref(input.channel.trim(), input.source_ref.as_deref())?;
    if input.channel.trim() == store::communications::channel::CALL && input.acknowledges.is_some()
    {
        return Err(ApiError::CommunicationInvalid(
            "only inbound email can acknowledge receipt".into(),
        ));
    }
    let row = match input.channel.trim() {
        store::communications::channel::CALL => store::communications::record_call(
            &state.surreal,
            &store::communications::NewCall {
                project_id: id,
                author_person_id: session.person_id,
                counterparty: input.counterparty,
                subject: input.subject,
                body: input.body,
                at: input.at,
                call: store::communications::CallDetails {
                    direction: input.direction.trim().to_string(),
                    counterparty_role: input
                        .role
                        .unwrap_or_else(|| {
                            store::communications::counterparty_role::OTHER.to_string()
                        })
                        .trim()
                        .to_string(),
                    duration_minutes: input.duration_minutes,
                    recorded: input
                        .recorded
                        .unwrap_or_else(|| store::communications::recorded::NONE.to_string())
                        .trim()
                        .to_string(),
                    meet_and_confer: input.meet_and_confer,
                },
                agreements: input.agreements,
                client_visible: input.client_visible,
            },
        )
        .await
        .map_err(map_communication)?,
        channel @ (store::communications::channel::EMAIL_INBOUND
        | store::communications::channel::EMAIL_OUTBOUND) => {
            create_email(&state, session.person_id, id, channel, source_ref, &input).await?
        }
        other => {
            return Err(ApiError::CommunicationInvalid(format!(
                "`{other}` cannot be logged here; use `call`, `email_inbound`, or `email_outbound`"
            )))
        }
    };
    Ok((StatusCode::CREATED, Json(row)).into_response())
}

#[cfg(test)]
mod tests {
    use super::{
        attachment_views, inbox_message_id, map_communication, validate_direction,
        validate_source_ref,
    };
    use axum::{http::StatusCode, response::IntoResponse};
    use uuid::Uuid;

    #[test]
    fn cross_project_source_reference_conflict_is_a_client_error() {
        let response = map_communication(store::communications::CommunicationError::Invalid(
            "source reference already belongs to another Project".into(),
        ))
        .into_response();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn source_reference_is_bounded_and_email_only() {
        assert_eq!(
            validate_source_ref("email_inbound", Some(" ref ")).unwrap(),
            Some("ref")
        );
        assert!(validate_source_ref("email_outbound", Some(" ")).is_err());
        assert!(validate_source_ref("email_inbound", Some(&"x".repeat(513))).is_err());
        assert!(validate_source_ref("call", Some("ref")).is_err());
        assert_eq!(validate_source_ref("call", None).unwrap(), None);
    }

    #[test]
    fn direction_filter_accepts_only_the_three_directions() {
        for direction in ["inbound", "outbound", "internal"] {
            assert_eq!(
                validate_direction(Some(direction)).unwrap(),
                Some(direction)
            );
        }
        assert_eq!(
            validate_direction(Some(" inbound ")).unwrap(),
            Some("inbound")
        );
        assert_eq!(validate_direction(None).unwrap(), None);
        assert_eq!(validate_direction(Some("")).unwrap(), None);
        // Unknown values are a client error, not a silent empty list.
        assert!(validate_direction(Some("sent")).is_err());
    }

    #[test]
    fn only_inbox_projections_carry_a_message_id() {
        let id = Uuid::now_v7();
        assert_eq!(
            inbox_message_id(&format!("inbox:{id}")).map(str::to_string),
            Some(id.to_string())
        );
        assert_eq!(inbox_message_id("gmail:synthetic/receipt"), None);
        assert_eq!(inbox_message_id("inbox:not-a-uuid"), None);
        assert_eq!(inbox_message_id("inbox:"), None);
    }

    #[test]
    fn attachments_dedupe_the_first_slot_and_keep_list_order() {
        let primary_id = Uuid::now_v7();
        let filed_id = Uuid::now_v7();
        let primary = store::assets::Asset {
            id: primary_id,
            filename: Some("exhibit-a.pdf".into()),
            kind: Some("unclassified".into()),
            ..asset_fixture()
        };
        let second = store::assets::Asset {
            id: filed_id,
            filename: Some("exhibit-b.pdf".into()),
            kind: Some("unclassified".into()),
            ..asset_fixture()
        };
        // The first attachment occupies both the row's asset slot and the
        // provenance set (AC8) — it must list once.
        let filed = vec![primary.clone(), second.clone()];
        let attachments = attachment_views(Some(primary.clone()), filed);
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].asset_id, primary_id);
        assert_eq!(attachments[0].filename.as_deref(), Some("exhibit-a.pdf"));
        assert_eq!(attachments[1].asset_id, filed_id);

        // A row with no Inbox projection lists only its own asset.
        assert_eq!(attachment_views(Some(primary.clone()), Vec::new()).len(), 1);
        assert!(attachment_views(None, Vec::new()).is_empty());
    }

    fn asset_fixture() -> store::assets::Asset {
        // Only the fields attachment_views reads are load-bearing; the
        // struct is a plain record, so the rest holds neutral values.
        serde_json::from_value(serde_json::json!({
            "id": Uuid::now_v7(),
            "storage_key": "assets/test",
            "content_type": "application/pdf",
            "byte_size": 1,
            "sha256_hex": "abc",
            "visibility": "internal",
            "notarization": [],
            "inserted_at": "2026-10-30T00:00:00Z",
            "updated_at": "2026-10-30T00:00:00Z"
        }))
        .unwrap()
    }
}
