//! Docket Entry doors (LAW-133).
//!
//! The read returns every case on the Project with every entry on each
//! case's docket. A write records one entry and returns the stored row; a
//! Project with no case yet gets one opened from the docket number the
//! caller names. Linking a document ends an entry's *source pending* state,
//! and a continuance supersedes the hearing or trial it moves. Nothing here
//! derives a date: `scheduled_on` is stored as the court set it.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use store::cases::{Case, DocketEntry, EntryKind};
use store::persons::Role;
use uuid::Uuid;

use crate::api::{ApiError, ApiState, LawyerSession};

/// `GET /app/api/projects/{id}/docket` — the cases and their entries.
#[derive(Debug, Serialize)]
pub(crate) struct ProjectDocket {
    pub(crate) cases: Vec<Case>,
    pub(crate) entries: Vec<DocketEntry>,
}

/// `POST /app/api/projects/{id}/docket`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordEntryRequest {
    /// The case this entry belongs to, by the forum's own docket number.
    /// Optional only while the Project has exactly one case.
    docket_number: Option<String>,
    /// The caption a case opened on this write carries; the docket number
    /// stands in when absent.
    caption: Option<String>,
    forum: Option<String>,
    jurisdiction: Option<String>,
    judge: Option<String>,
    /// Which side the client is on when a case is opened here.
    posture: Option<String>,
    entry_number: String,
    kind: String,
    title: String,
    party: Option<String>,
    filed_or_served_on: Option<String>,
    /// RFC 3339. Required for a hearing or trial, refused otherwise.
    scheduled_on: Option<String>,
    document_id: Option<Uuid>,
    /// The filed document by its slug below `documents/`, instead of its id.
    slug: Option<String>,
}

/// `POST /app/api/projects/{id}/docket/{entry_id}/link-document`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LinkDocumentRequest {
    document_id: Option<Uuid>,
    slug: Option<String>,
}

/// `POST /app/api/projects/{id}/docket/{entry_id}/supersede`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SupersedeRequest {
    entry_number: String,
    /// RFC 3339: the date the appearance moved to.
    scheduled_on: String,
    title: Option<String>,
    party: Option<String>,
    filed_or_served_on: Option<String>,
    document_id: Option<Uuid>,
    slug: Option<String>,
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

fn map_case(error: store::cases::CaseError) -> ApiError {
    match error {
        store::cases::CaseError::UnknownEntryKind(kind) => ApiError::DocketInvalid(format!(
            "`{kind}` is not a docket entry kind; accepted kinds are {}",
            kinds()
        )),
        store::cases::CaseError::NotAnAppearance(kind) => ApiError::DocketInvalid(format!(
            "a `{kind}` entry is not a hearing or trial, so it has no date to continue"
        )),
        store::cases::CaseError::EntryNotFound(_) => ApiError::NotFound,
        other => ApiError::Db(other.to_string()),
    }
}

fn kinds() -> String {
    EntryKind::ALL
        .iter()
        .map(|kind| kind.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_instant(value: &str, field: &str) -> Result<DateTime<Utc>, ApiError> {
    DateTime::parse_from_rfc3339(value.trim())
        .map(|instant| instant.with_timezone(&Utc))
        .map_err(|_| {
            ApiError::DocketInvalid(format!(
                "{field} must be an RFC 3339 timestamp with an explicit offset"
            ))
        })
}

fn non_blank(value: &str, field: &str) -> Result<String, ApiError> {
    let value = value.trim();
    if value.is_empty() {
        Err(ApiError::DocketInvalid(format!(
            "{field} must not be blank"
        )))
    } else {
        Ok(value.to_string())
    }
}

/// The document a request names, as an asset of this Project. A document
/// filed on another Project is refused as invalid rather than silently
/// linked across matters.
async fn resolve_document(
    state: &ApiState,
    project_id: Uuid,
    document_id: Option<Uuid>,
    slug: Option<&str>,
) -> Result<Option<Uuid>, ApiError> {
    if let Some(id) = document_id {
        let asset = store::assets::find_by_id(&state.surreal, id)
            .await
            .map_err(ApiError::Asset)?;
        return match asset {
            Some(asset) if asset.project_id == Some(project_id) => Ok(Some(asset.id)),
            _ => Err(ApiError::DocketInvalid(
                "document_id names no document on this Project".into(),
            )),
        };
    }
    if let Some(slug) = slug.map(str::trim).filter(|slug| !slug.is_empty()) {
        let asset = store::assets::current(
            &state.surreal,
            project_id,
            slug,
            store::assets::Lens::Lawyer,
        )
        .await
        .map_err(ApiError::Asset)?;
        return asset.map(|asset| Some(asset.id)).ok_or_else(|| {
            ApiError::DocketInvalid(format!("`{slug}` names no document on this Project"))
        });
    }
    Ok(None)
}

/// The case an entry joins: the one the docket number names, the Project's
/// only case when no number is given, or a case opened now from the number.
async fn resolve_case(
    state: &ApiState,
    project_id: Uuid,
    input: &RecordEntryRequest,
) -> Result<Case, ApiError> {
    let docket_number = input
        .docket_number
        .as_deref()
        .map(str::trim)
        .filter(|number| !number.is_empty());
    if let Some(number) = docket_number {
        if let Some(case) =
            store::cases::find_case_by_docket_number(&state.surreal, project_id, number)
                .await
                .map_err(map_case)?
        {
            return Ok(case);
        }
        let caption = input
            .caption
            .as_deref()
            .map(str::trim)
            .filter(|caption| !caption.is_empty())
            .unwrap_or(number);
        return store::cases::open_case(
            &state.surreal,
            &store::cases::NewCase {
                project_id,
                caption,
                forum: input.forum.as_deref(),
                jurisdiction: input.jurisdiction.as_deref(),
                docket_number: Some(number),
                judge: input.judge.as_deref(),
                posture: input.posture.as_deref().unwrap_or("unknown"),
            },
        )
        .await
        .map_err(map_case);
    }
    let mut cases = store::cases::for_project(&state.surreal, project_id)
        .await
        .map_err(map_case)?;
    match cases.len() {
        1 => Ok(cases.remove(0)),
        0 => Err(ApiError::DocketInvalid(
            "this Project has no case yet; name docket_number to open one".into(),
        )),
        _ => Err(ApiError::DocketInvalid(
            "this Project has several cases; name docket_number".into(),
        )),
    }
}

/// The entry, when it sits on one of this Project's cases; otherwise the
/// non-disclosing 404.
async fn entry_on_project(
    state: &ApiState,
    project_id: Uuid,
    entry_id: Uuid,
) -> Result<DocketEntry, ApiError> {
    let entry = store::cases::find_entry(&state.surreal, entry_id)
        .await
        .map_err(map_case)?
        .ok_or(ApiError::NotFound)?;
    let on_project = store::cases::for_project(&state.surreal, project_id)
        .await
        .map_err(map_case)?
        .iter()
        .any(|case| case.id == entry.case_id);
    if on_project {
        Ok(entry)
    } else {
        Err(ApiError::NotFound)
    }
}

/// `GET /app/api/projects/{id}/docket`
pub(crate) async fn read_docket(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let cases = store::cases::for_project(&state.surreal, id)
        .await
        .map_err(map_case)?;
    let entries = store::cases::docket_for_project(&state.surreal, id)
        .await
        .map_err(map_case)?;
    Ok(Json(ProjectDocket { cases, entries }).into_response())
}

/// `POST /app/api/projects/{id}/docket`
pub(crate) async fn record_entry(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path(id): Path<Uuid>,
    Json(input): Json<RecordEntryRequest>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    let kind = EntryKind::parse(input.kind.trim()).ok_or_else(|| {
        ApiError::DocketInvalid(format!(
            "`{}` is not a docket entry kind; accepted kinds are {}",
            input.kind,
            kinds()
        ))
    })?;
    let entry_number = non_blank(&input.entry_number, "entry_number")?;
    let title = non_blank(&input.title, "title")?;
    let scheduled_on = match (kind.is_appearance(), input.scheduled_on.as_deref()) {
        (true, Some(value)) => Some(parse_instant(value, "scheduled_on")?),
        (true, None) => {
            return Err(ApiError::DocketInvalid(format!(
                "a `{}` entry needs scheduled_on, the date the court set",
                kind.as_str()
            )))
        }
        (false, Some(_)) => {
            return Err(ApiError::DocketInvalid(format!(
                "a `{}` entry is not an appearance and carries no scheduled_on",
                kind.as_str()
            )))
        }
        (false, None) => None,
    };
    let document_asset_id =
        resolve_document(&state, id, input.document_id, input.slug.as_deref()).await?;
    let case = resolve_case(&state, id, &input).await?;
    // The same entry number on the same case is the same entry: an ingest
    // that replays a notice reads the row it recorded before.
    if let Some(existing) = store::cases::docket(&state.surreal, case.id)
        .await
        .map_err(map_case)?
        .into_iter()
        .find(|entry| entry.entry_number == entry_number)
    {
        return Ok((StatusCode::OK, Json(existing)).into_response());
    }
    let row = store::cases::record_entry(
        &state.surreal,
        &store::cases::NewDocketEntry {
            case_id: case.id,
            entry_number: &entry_number,
            kind,
            title: &title,
            party: input.party.as_deref().map(str::trim),
            filed_or_served_on: input.filed_or_served_on.as_deref().map(str::trim),
            scheduled_on,
            supersedes: None,
            document_asset_id,
            notation_id: None,
        },
    )
    .await
    .map_err(map_case)?;
    Ok((StatusCode::CREATED, Json(row)).into_response())
}

/// `POST /app/api/projects/{id}/docket/{entry_id}/link-document`
pub(crate) async fn link_document(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, entry_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<LinkDocumentRequest>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    entry_on_project(&state, id, entry_id).await?;
    let document = resolve_document(&state, id, input.document_id, input.slug.as_deref())
        .await?
        .ok_or_else(|| {
            ApiError::DocketInvalid("name the document by document_id or slug".into())
        })?;
    let row = store::cases::link_document(&state.surreal, entry_id, document)
        .await
        .map_err(map_case)?;
    Ok(Json(row).into_response())
}

/// `POST /app/api/projects/{id}/docket/{entry_id}/supersede`
pub(crate) async fn supersede_entry(
    State(state): State<ApiState>,
    LawyerSession(session): LawyerSession,
    Path((id, entry_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<SupersedeRequest>,
) -> Result<Response, ApiError> {
    require_participant(&state, session.person_id, session.role, id).await?;
    entry_on_project(&state, id, entry_id).await?;
    let entry_number = non_blank(&input.entry_number, "entry_number")?;
    let scheduled_on = parse_instant(&input.scheduled_on, "scheduled_on")?;
    let document_asset_id =
        resolve_document(&state, id, input.document_id, input.slug.as_deref()).await?;
    let row = store::cases::supersede(
        &state.surreal,
        entry_id,
        &store::cases::Continuance {
            entry_number: &entry_number,
            scheduled_on,
            title: input.title.as_deref().map(str::trim),
            party: input.party.as_deref().map(str::trim),
            filed_or_served_on: input.filed_or_served_on.as_deref().map(str::trim),
            document_asset_id,
        },
    )
    .await
    .map_err(map_case)?;
    Ok((StatusCode::CREATED, Json(row)).into_response())
}
