//! Helpers for the `playbook` table — a client Entity's stored
//! contract-negotiation positions.
//!
//! The `positions` column is JSONB; this module owns the typed view
//! ([`Position`]) and the (de)serialization, so `web` and the
//! contract-review analysis reach a `Vec<Position>` rather than a raw
//! `serde_json::Value`. A playbook is scoped to the client Entity, so one
//! playbook serves every matter for that client.
//!
//! Commands use PostgreSQL whenever its host is configured; see
//! [storage models](../../docs/storage-models.md#matter-records).

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::matter;
use serde_json::json;

const TABLE: &str = "playbook";

/// Severity a deviation from this position carries: `low` | `medium` |
/// `high`. Descriptive — used to rank findings, not enforced.
pub const SEVERITY_LOW: &str = "low";
pub const SEVERITY_MEDIUM: &str = "medium";
pub const SEVERITY_HIGH: &str = "high";

/// One stored position in a playbook — the firm's stance on a single
/// contract topic, with its fallback and walk-away lines.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    /// The contract topic this position governs (e.g. `Limitation of
    /// liability`, `Auto-renewal`, `Governing law`).
    pub topic: String,
    /// The preferred outcome the client wants.
    pub preferred: String,
    /// The acceptable fallback if the preferred outcome is refused.
    pub fallback: String,
    /// The line past which the client should not sign.
    pub walkaway: String,
    /// Severity of a deviation: see the `SEVERITY_*` constants.
    pub severity: String,
}

/// One `playbook` row: a client Entity's stored contract-negotiation
/// positions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Playbook {
    pub id: Uuid,
    pub entity_id: Uuid,
    pub name: String,
    /// The JSONB positions array — see [`positions_of`] for the typed
    /// view. SQL NULL retains the legacy JSON-null view without rewriting storage.
    #[serde(default)]
    pub positions: Json,
    pub active: bool,
    pub inserted_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Why a playbook command refused.
#[derive(Debug, thiserror::Error)]
pub enum PlaybookError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a playbook returned no usable row")]
    WriteReturnedNothing,
    /// The JSON (de)serialization of `positions` failed — a schema/data
    /// drift, never expected at runtime.
    #[error("playbook positions JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// `create` was given a `(entity_id, name)` pair that already has a
    /// playbook — the `playbook_entity_name` unique index refused it.
    #[error("a playbook named `{0}` already exists for this entity")]
    DuplicateName(String),
    /// [`update_positions`] was given an `id` that does not exist.
    #[error("playbook `{0}` not found")]
    NotFound(Uuid),
}

/// What to record for one new playbook.
#[derive(Debug, Clone)]
pub struct NewPlaybook<'a> {
    pub entity_id: Uuid,
    /// Human label, unique per Entity (e.g. `SaaS vendor MSA`).
    pub name: &'a str,
    pub positions: &'a [Position],
}

/// Insert one active `playbook` row, returning its id.
///
/// # Errors
///
/// [`PlaybookError::DuplicateName`] if `(entity_id, name)` already has a
/// playbook, or a database error.
pub async fn create(_db: &ApplicationStore, new: &NewPlaybook<'_>) -> Result<Uuid, PlaybookError> {
    let pg = matter::authority_for(TABLE).await?;
    let now = chrono::Utc::now();
    let row = Playbook {
        id: Uuid::now_v7(),
        entity_id: new.entity_id,
        name: new.name.to_owned(),
        positions: serde_json::to_value(new.positions)?,
        active: true,
        inserted_at: now,
        updated_at: now,
    };
    match matter::upsert::<Playbook>(&pg, TABLE, &row).await {
        Ok(stored) => Ok(stored.id),
        Err(matter::MatterAuthorityError::Conflict(_)) => {
            Err(PlaybookError::DuplicateName(new.name.to_owned()))
        }
        Err(error) => Err(error.into()),
    }
}

/// Load one playbook by id.
///
/// # Errors
///
/// Propagates any database error.
pub async fn by_id(_db: &ApplicationStore, id: Uuid) -> Result<Option<Playbook>, PlaybookError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::find(&pg, TABLE, json!({"id": id})).await?)
}

/// All playbooks for an Entity, name order.
///
/// # Errors
///
/// Propagates any database error.
pub async fn for_entity(
    _db: &ApplicationStore,
    entity_id: Uuid,
) -> Result<Vec<Playbook>, PlaybookError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::select(&pg, TABLE, json!({"entity_id": entity_id}), &["name"]).await?)
}

/// Every playbook, name order — the lawyer `/app/admin/playbooks` listing,
/// which is not scoped to one Entity.
///
/// # Errors
///
/// Propagates any database error.
pub async fn all(_db: &ApplicationStore) -> Result<Vec<Playbook>, PlaybookError> {
    let pg = matter::authority_for(TABLE).await?;
    Ok(matter::select(&pg, TABLE, json!({}), &["name"]).await?)
}

/// Replace a playbook's positions (the admin editor saves the whole set).
///
/// # Errors
///
/// [`PlaybookError::NotFound`] when `id` does not exist, or a database
/// error.
pub async fn update_positions(
    _db: &ApplicationStore,
    id: Uuid,
    positions: &[Position],
) -> Result<(), PlaybookError> {
    let pg = matter::authority_for(TABLE).await?;
    let changed: Vec<Playbook> = matter::update(
        &pg,
        TABLE,
        json!({"id": id}),
        json!({"positions": positions, "updated_at": chrono::Utc::now()}),
    )
    .await?;
    if changed.is_empty() {
        return Err(PlaybookError::NotFound(id));
    }
    Ok(())
}

/// The typed positions stored on a playbook row.
///
/// # Errors
///
/// Returns a JSON error if the stored `positions` value is not a
/// `Vec<Position>` (a schema/data drift, never expected at runtime).
pub fn positions_of(playbook: &Playbook) -> Result<Vec<Position>, serde_json::Error> {
    serde_json::from_value(playbook.positions.clone())
}

/// Render a position set into the pipe-delimited form the admin playbook
/// textarea holds — one `topic | preferred | fallback | walk-away | severity`
/// line per position.
///
/// Lives beside [`Position`] because both the page that prefills the textarea
/// (`webapp::playbooks`) and the handler that parses it back
/// (`portal::admin_playbooks::parse_positions`) need the same rendering, and a
/// second copy would be free to drift from the parser it round-trips with.
#[must_use]
pub fn positions_to_text(positions: &[Position]) -> String {
    positions
        .iter()
        .map(|p| {
            format!(
                "{} | {} | {} | {} | {}",
                p.topic, p.preferred, p.fallback, p.walkaway, p.severity
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {}
