//! PostgreSQL reads for Firm Calendar Settings and Project Calendar discovery.

use super::FirmError;
use uuid::Uuid;

/// Resolve the Firm's saved Google Workspace Calendar owner or its eligible bootstrap mailbox.
pub async fn calendar_owner(firm_id: Uuid) -> Result<Option<String>, FirmError> {
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(|_| FirmError::CalendarSettingsUnavailable)?;
    Ok(calendar_owners_query()
        .push("SELECT owner FROM calendar_owners WHERE owner IS NOT NULL AND firm_id = ")
        .push_bind(firm_id)
        .build_query_scalar()
        .fetch_optional(db.pool())
        .await?)
}

/// A saved settings row, including an explicit opt-out, takes precedence over the bootstrap owner.
pub(crate) fn calendar_owners_query() -> sqlx::QueryBuilder<sqlx::Postgres> {
    calendar_owners_with_bootstrap(std::env::var(cloud::calendar::CALENDAR_OWNER_ENV).ok())
}

pub(super) fn calendar_owners_with_bootstrap(
    bootstrap: Option<String>,
) -> sqlx::QueryBuilder<sqlx::Postgres> {
    let name = crate::seed::FIRM_ENTITY_NAME;
    let mut query = sqlx::QueryBuilder::new(
        "WITH calendar_owners AS (
            SELECT f.id AS firm_id, CASE
                WHEN f.workspace_provider <> 'google_workspace' THEN NULL
                WHEN s.firm_id IS NOT NULL THEN s.google_workspace_owner
                WHEN e.firm_anchor_key = ",
    );
    query
        .push_bind(crate::entity_commands::firm_anchor_key(name, name))
        .push(" THEN ")
        .push_bind(bootstrap.filter(|mailbox| !mailbox.trim().is_empty()))
        .push(
            " END AS owner
            FROM firm f
            LEFT JOIN firm_calendar_settings s ON s.firm_id = f.id
            LEFT JOIN entity e ON e.id = f.entity_id) ",
        );
    query
}

/// The Firms among `firm_ids` whose Owner chose Google Workspace as the main
/// email by saving a Calendar owner mailbox. Calendar surfaces exist only for
/// these Firms.
///
/// # Errors
/// [`FirmError`] when a Firm's Calendar settings cannot be read.
pub async fn with_google_calendar(firm_ids: Vec<Uuid>) -> Result<Vec<Uuid>, FirmError> {
    if firm_ids.is_empty() {
        return Ok(Vec::new());
    }
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(|_| FirmError::CalendarSettingsUnavailable)?;
    Ok(calendar_owners_query()
        .push("SELECT c.firm_id FROM unnest(")
        .push_bind(firm_ids)
        .push(
            "::uuid[]) WITH ORDINALITY AS requested(id, position)
               JOIN calendar_owners c ON c.firm_id = requested.id
               WHERE c.owner IS NOT NULL ORDER BY requested.position",
        )
        .build_query_scalar()
        .fetch_all(db.pool())
        .await?)
}
