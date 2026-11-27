//! PostgreSQL reads and writes for a Firm's Google Drive switch.

use super::{authorize_manage_firm, FirmError};
use crate::persons::Role;
use crate::surreal::SurrealDb;
use uuid::Uuid;

/// Whether the Firm's Projects use Google Drive folders. A Firm without a saved choice has Drive off.
///
/// # Errors
/// [`FirmError::DriveSettingsUnavailable`] when PostgreSQL cannot be reached.
pub async fn google_drive(firm_id: Uuid) -> Result<bool, FirmError> {
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(|_| FirmError::DriveSettingsUnavailable)?;
    Ok(
        sqlx::query_scalar("SELECT google_drive FROM firm_drive_settings WHERE firm_id = $1")
            .bind(firm_id)
            .fetch_optional(db.pool())
            .await?
            .unwrap_or(false),
    )
}

/// Save the Owner's Google Drive choice for the Firm.
///
/// # Errors
/// [`FirmError::NotAuthorized`] for anyone but an Owner who manages the Firm, and
/// [`FirmError::DriveSettingsUnavailable`] when PostgreSQL cannot be reached.
pub async fn set_google_drive(
    surreal: &SurrealDb,
    actor_role: Role,
    actor_person_id: Option<Uuid>,
    firm_id: Uuid,
    enabled: bool,
) -> Result<(), FirmError> {
    if actor_role != Role::Owner {
        return Err(FirmError::NotAuthorized);
    }
    authorize_manage_firm(surreal, actor_role, actor_person_id, firm_id).await?;
    let db = crate::postgres::PostgresDb::application()
        .await
        .map_err(|_| FirmError::DriveSettingsUnavailable)?;
    sqlx::query(
        "INSERT INTO firm_drive_settings (firm_id, google_drive) VALUES ($1, $2)
         ON CONFLICT (firm_id) DO UPDATE SET google_drive = EXCLUDED.google_drive",
    )
    .bind(firm_id)
    .bind(enabled)
    .execute(db.pool())
    .await?;
    Ok(())
}
