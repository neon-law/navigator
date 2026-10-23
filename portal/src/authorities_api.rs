//! The `/app/api/authorities` door (ENG-712).
//!
//! One operation: create (or find) the global Authority for a citation,
//! archiving new citations through the Asset service and
//! recording the resulting asset id as `archived_asset_id`. Lawyer-tier
//! only — minting global legal reference data is firm-side authoring, the
//! same tier every other `/app/api` authoring door takes.
//!
//! Bytes travel base64-encoded in the JSON body, the same shape
//! `POST /app/api/projects/{id}/documents` uses, so the CLI (the only
//! caller today — see `cli::authorities`) needs no multipart client.
//!
//! `store::authorities::record` is already find-or-create on `citation`
//! (#890): a repeat of the same citation returns the existing global row
//! untouched, including its original `archived_asset_id`. This door adds
//! no new semantics there — it only reaches the seam, which is the whole
//! point of the issue: before this, nothing outside `store`'s own tests
//! called `record` at all.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::api::{ApiError, ApiState, LawyerSession};

/// `POST /app/api/authorities` request body. `archive_base64` is the
/// archived artifact's bytes; `content_type` defaults to
/// `application/octet-stream` when absent or blank, the same default
/// `POST /app/api/projects/{id}/documents` uses.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateAuthorityRequest {
    class: String,
    citation: String,
    title: String,
    short_cite: Option<String>,
    publisher: Option<String>,
    issued_on: Option<String>,
    canonical_url: Option<String>,
    checked_on: Option<String>,
    archive_base64: String,
    content_type: Option<String>,
}

/// A `400 Bad Request` JSON body in the shared `{error, message}` shape —
/// its own copy of `api::bad_request`, which is private to that module.
fn bad_request(error: &'static str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": error, "message": message })),
    )
        .into_response()
}

fn ambiguous_identity() -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "ambiguous_authority_identity",
            "message": "this citation is already filed under a different Authority class; review the citation before filing"
        })),
    )
        .into_response()
}

fn archive_mismatch() -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "authority_archive_mismatch",
            "message": "this citation is already filed with different archive bytes; review the Authority before filing"
        })),
    )
        .into_response()
}

/// A present, non-blank, trimmed value — `None` for an absent or
/// whitespace-only field. Every optional `NewAuthority` field goes through
/// this so a caller sending `""` for `short_cite` gets the same `None` a
/// caller who omitted the field entirely would.
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// `POST /app/api/authorities` — create (or find) the global Authority for
/// `citation`, archiving `archive_base64` for a new citation.
///
/// Lawyer-tier only; the [`LawyerSession`] extractor rejects an anonymous
/// (401) or non-lawyer (403) caller before the body is even parsed.
/// `400 invalid_class` when `class` is outside
/// [`rules::citation::AuthorityClass`]; `400 archive_unreadable` when the
/// archive is missing, not valid base64, or decodes to zero bytes. The
/// An exact citation already on file returns its existing row without
/// archiving duplicate bytes. A conflicting class is refused for review.
pub(crate) async fn create_authority_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Json(input): Json<CreateAuthorityRequest>,
) -> Result<Response, ApiError> {
    let Some(class) = rules::citation::AuthorityClass::parse(input.class.trim()) else {
        return Ok(bad_request(
            "invalid_class",
            &format!(
                "class must be one of: {}",
                rules::citation::AuthorityClass::ALL
                    .iter()
                    .map(|class| class.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    };

    let bytes = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(input.archive_base64.as_bytes())
            .ok()
            .filter(|bytes| !bytes.is_empty())
    };
    let Some(bytes) = bytes else {
        return Ok(bad_request(
            "archive_unreadable",
            "archive_base64 is missing, not valid base64, or decodes to zero bytes.",
        ));
    };

    let citation = input.citation.trim();
    if let Some(existing) = store::authorities::find(
        &state.surreal,
        store::authorities::AuthorityLookup::Citation(citation),
    )
    .await
    .map_err(|error| ApiError::Db(format!("authority lookup failed: {error}")))?
    {
        if existing.class != class.as_str() {
            return Ok(ambiguous_identity());
        }
        let archived = match existing.archived_asset_id {
            Some(id) => store::assets::find_by_id(&state.surreal, id)
                .await
                .map_err(|error| {
                    ApiError::Db(format!("authority archive lookup failed: {error}"))
                })?,
            None => None,
        };
        let Some(archived) = archived else {
            return Ok(archive_mismatch());
        };
        if archived.sha256_hex != store::assets::sha256_hex(&bytes)
            || archived.byte_size != i64::try_from(bytes.len()).unwrap_or(i64::MAX)
        {
            return Ok(archive_mismatch());
        }
        return Ok(Json(existing).into_response());
    }

    let content_type = input
        .content_type
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("application/octet-stream");
    let asset_id =
        match store::assets::ingest_content(&state.surreal, &state.storage, &bytes, content_type)
            .await
        {
            Ok(id) => id,
            Err(error) => {
                tracing::error!(error = %error, "api: authority archive ingest failed");
                return Err(ApiError::Db(
                    "the archived artifact could not be persisted".to_string(),
                ));
            }
        };

    let new = store::authorities::NewAuthority {
        class,
        citation,
        short_cite: trimmed(input.short_cite.as_deref()),
        title: input.title.trim(),
        publisher: trimmed(input.publisher.as_deref()),
        issued_on: trimmed(input.issued_on.as_deref()),
        canonical_url: trimmed(input.canonical_url.as_deref()),
        checked_on: trimmed(input.checked_on.as_deref()),
        archived_asset_id: Some(asset_id),
    };
    match store::authorities::record(&state.surreal, &new).await {
        Ok(authority) if authority.class != class.as_str() => Ok(ambiguous_identity()),
        Ok(authority) => Ok(Json(authority).into_response()),
        Err(error) => {
            tracing::error!(error = %error, "api: authority record failed");
            Err(ApiError::Db(
                "the authority could not be recorded".to_string(),
            ))
        }
    }
}

/// `PATCH /app/api/authorities` request body (LAW-61). Exactly one of `id`
/// or `citation` locates the Authority to update; every other field is
/// optional and left unchanged when absent. There is deliberately no
/// `class` or (mutating) `citation` field — see
/// [`store::authorities::AuthorityLookup`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateAuthorityRequest {
    id: Option<uuid::Uuid>,
    citation: Option<String>,
    title: Option<String>,
    short_cite: Option<String>,
    publisher: Option<String>,
    issued_on: Option<String>,
    canonical_url: Option<String>,
    checked_on: Option<String>,
    /// A new artifact version to archive in place of the current one.
    archive_base64: Option<String>,
    content_type: Option<String>,
}

/// `PATCH /app/api/authorities` — correct a field on an existing Authority
/// (LAW-61). Lawyer-tier only, the same gate as [`create_authority_door`].
///
/// `400 identifier_required` when neither `id` nor `citation` is given;
/// `400 ambiguous_identifier` when both are; `404 not_found` when the
/// identifier matches no row. `--citation` and `class` are not accepted
/// here at all — they are the Authority's identity (see
/// [`store::authorities::AuthorityLookup`]), and changing either is a new
/// Authority, not an update of this one. When `archive_base64` is given, it
/// is ingested as a new content asset and replaces `archived_asset_id`;
/// the previous archive is left in place (assets are never deleted), so a
/// caller who kept the old asset id can still reach the earlier bytes.
pub(crate) async fn update_authority_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    Json(input): Json<UpdateAuthorityRequest>,
) -> Result<Response, ApiError> {
    let lookup = match (input.id, trimmed(input.citation.as_deref())) {
        (Some(id), None) => store::authorities::AuthorityLookup::Id(id),
        (None, Some(citation)) => store::authorities::AuthorityLookup::Citation(citation),
        (None, None) => {
            return Ok(bad_request(
                "identifier_required",
                "one of id or citation is required",
            ))
        }
        (Some(_), Some(_)) => {
            return Ok(bad_request(
                "ambiguous_identifier",
                "give id or citation, not both",
            ))
        }
    };

    let archived_asset_id = match trimmed(input.archive_base64.as_deref()) {
        Some(encoded) => {
            let bytes = {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD
                    .decode(encoded.as_bytes())
                    .ok()
                    .filter(|bytes| !bytes.is_empty())
            };
            let Some(bytes) = bytes else {
                return Ok(bad_request(
                    "archive_unreadable",
                    "archive_base64 is not valid base64, or decodes to zero bytes.",
                ));
            };
            let content_type =
                trimmed(input.content_type.as_deref()).unwrap_or("application/octet-stream");
            match store::assets::ingest_content(
                &state.surreal,
                &state.storage,
                &bytes,
                content_type,
            )
            .await
            {
                Ok(id) => Some(id),
                Err(error) => {
                    tracing::error!(error = %error, "api: authority archive ingest failed");
                    return Err(ApiError::Db(
                        "the archived artifact could not be persisted".to_string(),
                    ));
                }
            }
        }
        None => None,
    };

    let patch = store::authorities::AuthorityPatch {
        title: trimmed(input.title.as_deref()),
        short_cite: trimmed(input.short_cite.as_deref()),
        publisher: trimmed(input.publisher.as_deref()),
        issued_on: trimmed(input.issued_on.as_deref()),
        canonical_url: trimmed(input.canonical_url.as_deref()),
        checked_on: trimmed(input.checked_on.as_deref()),
        archived_asset_id,
    };

    match store::authorities::update(&state.surreal, lookup, &patch).await {
        Ok(authority) => Ok(Json(authority).into_response()),
        Err(store::authorities::AuthorityError::NotFound) => Ok((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "not_found",
                "message": "no authority matches that id or citation",
            })),
        )
            .into_response()),
        Err(error) => {
            tracing::error!(error = %error, "api: authority update failed");
            Err(ApiError::Db(
                "the authority could not be updated".to_string(),
            ))
        }
    }
}

/// `GET /app/api/authorities/{id}/archive` — the bytes of an Authority's
/// archived artifact, the read half of `POST /app/api/authorities` (LAW-93).
///
/// Lawyer-tier only, the same gate as the write. `404 not_found` when no
/// Authority has that id or it carries no archive. The body is served as an
/// opaque attachment: the declared type is the uploader's, so the response
/// forbids sniffing and sandboxes any rendering.
pub(crate) async fn read_authority_archive_door(
    State(state): State<ApiState>,
    _lawyer: LawyerSession,
    axum::extract::Path(id): axum::extract::Path<uuid::Uuid>,
) -> Result<Response, ApiError> {
    let not_found = || {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "not_found",
                "message": "no authority with that id has an archived artifact",
            })),
        )
            .into_response()
    };
    let authority =
        match store::authorities::find(&state.surreal, store::authorities::AuthorityLookup::Id(id))
            .await
        {
            Ok(found) => found,
            Err(error) => {
                tracing::error!(error = %error, "api: authority lookup failed");
                return Err(ApiError::Db("the authority could not be read".to_string()));
            }
        };
    let Some(asset_id) = authority.and_then(|authority| authority.archived_asset_id) else {
        return Ok(not_found());
    };
    let asset = match store::assets::find_by_id(&state.surreal, asset_id).await {
        Ok(Some(asset)) => asset,
        Ok(None) => return Ok(not_found()),
        Err(error) => {
            tracing::error!(error = %error, "api: authority archive lookup failed");
            return Err(ApiError::Db(
                "the archived artifact could not be read".to_string(),
            ));
        }
    };
    let object = match state.storage.get(&asset.storage_key).await {
        Ok(object) => object,
        Err(error) => {
            tracing::error!(error = %error, "api: authority archive read failed");
            return Err(ApiError::Db(
                "the archived artifact could not be read".to_string(),
            ));
        }
    };
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, asset.content_type),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment".to_string(),
            ),
            (
                axum::http::header::X_CONTENT_TYPE_OPTIONS,
                "nosniff".to_string(),
            ),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                "sandbox".to_string(),
            ),
        ],
        object.bytes,
    )
        .into_response())
}
