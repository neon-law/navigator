//! Communication doors (LAW-140).
//!
//! The read is the firm view of a Project's conversation log: every row,
//! internal notes and logged calls included, at Lawyer tier and scoped to a
//! participant. A client reads the conversation through
//! `GET .../conversation`, which excludes internal notes in the query. The
//! write logs a call as an internal-note Communication with its structure,
//! or an email that confirms a call's agreements.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
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
    /// A call's `opposing_counsel`, `client`, `court`, or `other`.
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

/// `GET /app/api/projects/{id}/communications`
pub(crate) async fn list_communications(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let rows = store::communications::for_project(&state.surreal, id)
        .await
        .map_err(map_communication)?;
    Ok(Json(rows).into_response())
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
            let ingested = store::communications::ingest(
                &state.surreal,
                &store::communications::IngestArgs {
                    project_id: id,
                    channel,
                    direction,
                    author_person_id: session.person_id,
                    counterparty: Some(input.counterparty.trim()),
                    subject: input.subject.as_deref(),
                    body: input.body.trim(),
                    source_ref,
                    asset_id: None,
                    occurred_at: &at,
                },
            )
            .await
            .map_err(map_communication)?;
            if let Some(call_id) = input.confirms {
                store::communications::confirm_agreements(
                    &state.surreal,
                    id,
                    call_id,
                    ingested.communication_id,
                )
                .await
                .map_err(map_communication)?;
            }
            store::communications::find(&state.surreal, id, ingested.communication_id)
                .await
                .map_err(map_communication)?
                .ok_or_else(|| ApiError::Db("the email could not be read back".into()))?
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
    use super::{map_communication, validate_source_ref};
    use axum::{http::StatusCode, response::IntoResponse};

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
}
