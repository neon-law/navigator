//! PostgreSQL ownership and activation for Notation and Project Slack workflows.

use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// Errors intentionally omit captured values and provider payloads.
#[derive(Debug, Error)]
pub enum HandoverError {
    #[error("invalid source environment for DBOS activation")]
    InvalidEnvironment,
    #[error("DBOS ownership conflicts with existing state or is unavailable")]
    Conflict,
    #[error("a Notation has an unfinished native operation")]
    PendingNativeOperation,
    #[error("DBOS ownership storage unavailable")]
    Storage,
}

impl From<sqlx::Error> for HandoverError {
    fn from(_: sqlx::Error) -> Self {
        Self::Storage
    }
}

/// Serialize Project creation against complete-inventory activation. Call before
/// inserting a native Project; fixed-ID seeds take the lock but are not attested.
pub(crate) async fn hold_creation_epoch(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock_shared($1)")
        .bind(OWNERSHIP_LOCK)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

const OWNERSHIP_LOCK: i64 = -6_817_312_814_293_227_184;

/// Establish DBOS ownership without importing source objects. The operator
/// holds legacy writers and verifies the source has no unfinished work.
/// Existing Projects have unresolved historical Slack delivery evidence.
///
/// # Errors
/// Refuses another ownership epoch, an unfinished native transition, a partial
/// import, or unavailable storage. A repeat for the same source is a no-op.
pub async fn activate_without_capture(
    postgres: &PostgresDb,
    source_environment: &str,
) -> Result<i32, HandoverError> {
    if source_environment.trim() != source_environment
        || source_environment.is_empty()
        || source_environment.len() > 128
    {
        return Err(HandoverError::InvalidEnvironment);
    }
    let mut tx = postgres.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *tx)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(OWNERSHIP_LOCK)
        .execute(&mut *tx)
        .await?;
    if let Some((origin, environment, projects)) = sqlx::query_as::<_, (String, Option<String>, i32)>(
        "SELECT h.origin,h.source_environment,h.slack_projects FROM durable_object_ownership o \
         JOIN durable_object_handover h USING (capture_id) WHERE o.singleton",
    )
    .fetch_optional(&mut *tx)
    .await?
    {
        return if origin == "source_discarded" && environment.as_deref() == Some(source_environment) {
            Ok(projects)
        } else {
            Err(HandoverError::Conflict)
        };
    }
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM notation_transition WHERE result IS NULL)",
    )
    .fetch_one(&mut *tx)
    .await?
    {
        return Err(HandoverError::PendingNativeOperation);
    }
    if sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM durable_object_handover) \
         OR EXISTS(SELECT 1 FROM database_origin)",
    )
    .fetch_one(&mut *tx)
    .await?
    {
        return Err(HandoverError::Conflict);
    }
    let receipt = Uuid::new_v4();
    let epoch = Uuid::new_v4();
    let projects: i32 = sqlx::query_scalar(
        "INSERT INTO durable_object_handover \
         (capture_id,capture_version,origin,source_environment,notation_machines,slack_projects,slack_posted_events) \
         SELECT $1,1,'source_discarded',$2,0,count(*)::integer,0 FROM project RETURNING slack_projects",
    )
    .bind(receipt)
    .bind(source_environment)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO project_slack_handover \
         (project_id,ownership_epoch,capture_id,native_active,client_view_disposition) \
         SELECT id,$1,$2,true,'unresolved' FROM project",
    )
    .bind(epoch)
    .bind(receipt)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO durable_object_ownership(singleton,ownership_epoch,capture_id) VALUES (true,$1,$2)",
    )
    .bind(epoch)
    .bind(receipt)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(projects)
}

/// Require the independently activated complete retained-object handover before
/// native Notation effects or state reads. PostgreSQL domain authority is separate.
///
/// # Errors
/// Missing ownership and storage failures refuse native execution.
pub async fn require_native_ownership(postgres: &PostgresDb) -> Result<(), HandoverError> {
    let active: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM durable_object_ownership WHERE singleton)")
            .fetch_one(postgres.pool())
            .await?;
    if !active {
        return Err(HandoverError::Conflict);
    }
    Ok(())
}

/// Verify that PostgreSQL owns the durable objects before starting the application.
pub async fn verify_runtime_ownership(postgres: &PostgresDb) -> Result<(), HandoverError> {
    let fresh = crate::postgres::native_origin::admits(postgres)
        .await
        .map_err(|error| match error {
            crate::postgres::native_origin::OriginError::Storage => HandoverError::Storage,
            _ => HandoverError::Conflict,
        })?;
    let rows=sqlx::query("SELECT o.ownership_epoch,o.capture_id,h.capture_id AS header_id,h.capture_version,h.origin,h.source_environment,h.capture_sha256,h.notation_machines,h.slack_projects,h.slack_posted_events FROM durable_object_ownership o LEFT JOIN durable_object_handover h ON h.capture_id=o.capture_id")
        .fetch_all(postgres.pool()).await?;
    let [row] = rows.as_slice() else {
        return Err(HandoverError::Conflict);
    };
    let epoch: Uuid = row.try_get("ownership_epoch")?;
    let capture: Uuid = row.try_get("capture_id")?;
    let mode: Option<String> = row.try_get("origin")?;
    if epoch.is_nil()
        || capture.is_nil()
        || row.try_get::<Option<Uuid>, _>("header_id")? != Some(capture)
        || row.try_get::<Option<i32>, _>("capture_version")? != Some(1)
    {
        return Err(HandoverError::Conflict);
    }
    let environment: Option<String> = row.try_get("source_environment")?;
    let digest: Option<String> = row.try_get("capture_sha256")?;
    match mode.as_deref() {
        Some("fresh_native") => {
            if environment.is_some()
                || digest.is_some()
                || row.try_get::<i32, _>("notation_machines")? != 0
                || row.try_get::<i32, _>("slack_projects")? != 0
                || row.try_get::<i32, _>("slack_posted_events")? != 0
            {
                return Err(HandoverError::Conflict);
            }
        }
        Some("captured_source") if !fresh => {
            if environment
                .as_ref()
                .is_none_or(|value| value.is_empty() || value.len() > 128)
                || digest.as_ref().is_none_or(|value| {
                    value.len() != 64
                        || !value
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            {
                return Err(HandoverError::Conflict);
            }
        }
        Some("source_discarded") if !fresh => {
            if environment.as_ref().is_none_or(String::is_empty)
                || digest.is_some()
                || row.try_get::<i32, _>("notation_machines")? != 0
                || row.try_get::<i32, _>("slack_posted_events")? != 0
            {
                return Err(HandoverError::Conflict);
            }
        }
        _ => return Err(HandoverError::Conflict),
    }
    Ok(())
}

// Only the two exclusively-created resource constructors call this transaction
// seam; no public connection or environment setting can initialize ownership.
pub(crate) async fn initialize_fresh_on(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<(), sqlx::Error> {
    let capture = Uuid::new_v4();
    let epoch = Uuid::new_v4();
    sqlx::query("INSERT INTO durable_object_handover(capture_id,capture_version,origin,notation_machines,slack_projects,slack_posted_events) VALUES ($1,1,'fresh_native',0,0,0)")
        .bind(capture).execute(&mut **transaction).await?;
    sqlx::query("INSERT INTO durable_object_ownership(singleton,ownership_epoch,capture_id) VALUES (true,$1,$2)")
        .bind(epoch).bind(capture).execute(&mut **transaction).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_slack_deliveries::{self, DeliveryError};

    #[tokio::test]
    async fn no_capture_activates_once_and_refuses_unverified_historical_delivery() {
        crate::test_support::with_captured_postgres(async {
            let pg = PostgresDb::application().await.unwrap();
            let (historical, firm_id) = crate::test_support::seed_calendar_project().await;
            let notation: Uuid = sqlx::query_scalar(
                "WITH t AS (INSERT INTO template (id,code,title,respondent_type,is_current,inserted_at,updated_at) \
                 VALUES (gen_random_uuid(),'synthetic-cutover','Synthetic','person',false,now(),now()) RETURNING id), \
                 p AS (INSERT INTO person (name,email,role) VALUES ('Synthetic','cutover@example.test','client') RETURNING id) \
                 INSERT INTO notation (template_id,person_id,project_id,state) \
                 SELECT t.id,p.id,$1,'BEGIN' FROM t,p RETURNING id",
            )
            .bind(historical)
            .fetch_one(pg.pool())
            .await
            .unwrap();
            assert_eq!(activate_without_capture(&pg, "synthetic").await.unwrap(), 1);
            assert!(matches!(
                crate::notation_machine_state::start_machine(&pg, notation, "workflow", "synthetic", false).await,
                Err(crate::notation_machine_state::MachineStateError::MachineMissing { .. })
            ));
            assert!(matches!(
                project_slack_deliveries::admission(&pg, historical, "client_view", "client-view")
                    .await,
                Err(DeliveryError::Unresolved)
            ));
            assert!(matches!(
                project_slack_deliveries::admission(&pg, historical, "main_push", "abc1234:1")
                    .await,
                Err(DeliveryError::MainPushHistoryUnavailable)
            ));
            verify_runtime_ownership(&pg).await.unwrap();

            let entity_id = sqlx::query_scalar("SELECT entity_id FROM project WHERE id=$1")
                .bind(historical)
                .fetch_one(pg.pool())
                .await
                .unwrap();
            sqlx::query(
                "INSERT INTO brand (name,brand_key,firm_id) VALUES ('Synthetic Cutover','synthetic-cutover',$1)",
            )
            .bind(firm_id)
            .execute(pg.pool())
            .await
            .unwrap();
            let new_project = crate::postgres::projects::create_on(
                &pg,
                &crate::projects::NewProject {
                    code: "synthetic-after-cutover".into(),
                    name: "Synthetic Project".into(),
                    status: "open".into(),
                    brand: "synthetic-cutover".into(),
                    firm_id: Some(firm_id),
                    entity_id,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                project_slack_deliveries::admission(&pg, new_project.id, "main_push", "abc1234:1")
                    .await,
                Ok(project_slack_deliveries::Admission::Allowed)
            ));
            assert_eq!(activate_without_capture(&pg, "synthetic").await.unwrap(), 1);
            assert!(matches!(
                activate_without_capture(&pg, "other").await,
                Err(HandoverError::Conflict)
            ));
        })
        .await;
    }
}
