//! Atomic parity import for an offline retained-object capture.
//!
//! A parity receipt binds the input and target values. It does not authenticate
//! the source, prove an uninterrupted writer cutoff or activate native callers.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use thiserror::Error;
use uuid::Uuid;

use crate::{notation_machine_state::MachineState, postgres::PostgresDb};

/// Client-view posting evidence, independently reconciled from invocation effects.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientViewDisposition {
    Posted,
    NotPosted,
    Unresolved,
}

impl ClientViewDisposition {
    fn as_str(self) -> &'static str {
        match self {
            Self::Posted => "posted",
            Self::NotPosted => "not_posted",
            Self::Unresolved => "unresolved",
        }
    }
}

/// One Project's complete retained bounded event ring, not an unlimited history.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlackProject {
    pub project_id: Uuid,
    pub main_push_seen: Vec<String>,
    pub client_view: ClientViewDisposition,
}

/// A structurally checked capture whose specs are validated by the operator's
/// canonical workflow parser before calling this storage component.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capture {
    pub format_version: u32,
    pub capture_id: Uuid,
    pub source_environment: String,
    pub ownership_epoch: Uuid,
    pub source_readback_sha256: String,
    pub captured_at_unix: u64,
    pub notation_machines: Vec<MachineState>,
    pub slack_projects: Vec<SlackProject>,
}

/// Payload-free component parity receipt. No source-authenticity assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParityReceipt {
    pub format_version: u32,
    pub capture_id: Uuid,
    pub capture_sha256: String,
    pub notation_machines: usize,
    pub slack_projects: usize,
    pub slack_posted_events: usize,
    pub committed: bool,
    pub native_activation: bool,
}

/// Errors intentionally omit captured values and provider payloads.
#[derive(Debug, Error)]
pub enum HandoverError {
    #[error("invalid retained-object capture")]
    InvalidCapture,
    #[error("retained-object capture conflicts with native state")]
    Conflict,
    #[error("captured Notation has unfinished native operation ownership")]
    PendingNativeOperation,
    #[error("retained-object capture storage unavailable")]
    Storage,
}

impl From<sqlx::Error> for HandoverError {
    fn from(_: sqlx::Error) -> Self {
        Self::Storage
    }
}

impl Capture {
    /// Check bounded structure and canonical identities without logging contents.
    /// Spec semantics and capture provenance are checked by the operator adapter.
    ///
    /// # Errors
    /// The version, identity, bounds or retained event structure is invalid.
    pub fn validate(&self) -> Result<(), HandoverError> {
        if self.format_version != 1
            || self.capture_id.is_nil()
            || self.ownership_epoch.is_nil()
            || self.source_readback_sha256.len() != 64
            || !self
                .source_readback_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.captured_at_unix == 0
            || self.source_environment.is_empty()
            || self.source_environment.len() > 128
            || self.notation_machines.len() > 100_000
            || self.slack_projects.len() > 100_000
        {
            return Err(HandoverError::InvalidCapture);
        }
        let mut machines = BTreeSet::new();
        for machine in &self.notation_machines {
            if machine.notation_id.is_nil()
                || !matches!(machine.machine_kind.as_str(), "questionnaire" | "workflow")
                || machine.spec_yaml.is_empty()
                || machine.spec_yaml.len() > 1024 * 1024
                || machine.current_state.is_empty()
                || machine.current_state.len() > 512
                || !machines.insert((machine.notation_id, machine.machine_kind.as_str()))
            {
                return Err(HandoverError::InvalidCapture);
            }
        }
        let mut projects = BTreeSet::new();
        for project in &self.slack_projects {
            let mut events = BTreeSet::new();
            if project.project_id.is_nil()
                || !projects.insert(project.project_id)
                || project.main_push_seen.len() > 200
            {
                return Err(HandoverError::InvalidCapture);
            }
            for event in &project.main_push_seen {
                crate::project_slack_deliveries::validate_event("main_push", event)
                    .map_err(|_| HandoverError::InvalidCapture)?;
                if !events.insert(event) {
                    return Err(HandoverError::InvalidCapture);
                }
            }
        }
        Ok(())
    }
}

/// Compare and atomically import all captured rows without changing ownership.
/// Dry-run compares target rows in a read-only transaction and performs no writes.
///
/// # Errors
/// Rejects any conflicting target, changed capture identity or failed storage operation.
pub async fn import(
    postgres: &PostgresDb,
    capture: &Capture,
    apply: bool,
) -> Result<ParityReceipt, HandoverError> {
    capture.validate()?;
    let bytes = serde_json::to_vec(capture).map_err(|_| HandoverError::InvalidCapture)?;
    let digest = hex::encode(Sha256::digest(bytes));
    let mut transaction = postgres.pool().begin().await?;
    if !apply {
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *transaction)
            .await?;
    }
    hold_creation_epoch(&mut transaction).await?;
    let already_active = require_same_transfer(&mut transaction, capture.capture_id).await?;
    // These are the same keys held by ordinary native adapters. Stable combined
    // order avoids import/import deadlocks; duplicate keys need only one lock.
    let mut locks: BTreeSet<i64> = capture
        .notation_machines
        .iter()
        .map(|machine| crate::notation_machine_state::lane_key(machine.notation_id))
        .collect();
    locks.insert(crate::notation_machine_state::lane_key(capture.capture_id));
    locks.extend(
        capture
            .slack_projects
            .iter()
            .map(|project| crate::project_slack_deliveries::lane_key(project.project_id)),
    );
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *transaction)
        .await?;
    for lock in locks {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock)
            .execute(&mut *transaction)
            .await?;
    }
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT capture_sha256 FROM durable_object_handover WHERE capture_id=$1",
    )
    .bind(capture.capture_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if existing
        .as_ref()
        .is_some_and(|existing| existing != &digest)
    {
        return Err(HandoverError::Conflict);
    }
    let event_count = capture
        .slack_projects
        .iter()
        .map(|project| {
            project.main_push_seen.len()
                + usize::from(project.client_view == ClientViewDisposition::Posted)
        })
        .sum::<usize>();
    if apply {
        sqlx::query("INSERT INTO durable_object_handover (capture_id,capture_version,source_environment,capture_sha256,notation_machines,slack_projects,slack_posted_events) VALUES ($1,1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING")
        .bind(capture.capture_id).bind(&capture.source_environment).bind(&digest)
        .bind(i32::try_from(capture.notation_machines.len()).map_err(|_| HandoverError::InvalidCapture)?)
        .bind(i32::try_from(capture.slack_projects.len()).map_err(|_| HandoverError::InvalidCapture)?)
        .bind(i32::try_from(event_count).map_err(|_| HandoverError::InvalidCapture)?)
        .execute(&mut *transaction).await?;
    }
    import_notation_machines(&mut transaction, capture, apply).await?;
    import_slack_projects(&mut transaction, capture, apply, already_active).await?;
    if apply {
        transaction.commit().await?;
    } else {
        transaction.rollback().await?;
    }
    Ok(ParityReceipt {
        format_version: 1,
        capture_id: capture.capture_id,
        capture_sha256: digest,
        notation_machines: capture.notation_machines.len(),
        slack_projects: capture.slack_projects.len(),
        slack_posted_events: event_count,
        committed: apply,
        native_activation: false,
    })
}

async fn import_notation_machines(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    capture: &Capture,
    apply: bool,
) -> Result<(), HandoverError> {
    let notations = capture
        .notation_machines
        .iter()
        .map(|machine| machine.notation_id)
        .collect::<BTreeSet<_>>();
    for notation in notations {
        let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notation_transition WHERE notation_id=$1 AND result IS NULL)")
            .bind(notation).fetch_one(&mut **transaction).await?;
        if pending {
            return Err(HandoverError::PendingNativeOperation);
        }
    }
    for machine in &capture.notation_machines {
        if !machine.ephemeral {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notation WHERE id=$1)")
                    .bind(machine.notation_id)
                    .fetch_one(&mut **transaction)
                    .await?;
            if !exists {
                return Err(HandoverError::InvalidCapture);
            }
        }
        if apply {
            sqlx::query("INSERT INTO notation_machine_state (notation_id,machine_kind,spec_yaml,current_state,ephemeral) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING")
            .bind(machine.notation_id).bind(&machine.machine_kind).bind(&machine.spec_yaml)
            .bind(&machine.current_state).bind(machine.ephemeral).execute(&mut **transaction).await?;
        }
        let row = sqlx::query("SELECT spec_yaml,current_state,ephemeral FROM notation_machine_state WHERE notation_id=$1 AND machine_kind=$2")
            .bind(machine.notation_id).bind(&machine.machine_kind).fetch_optional(&mut **transaction).await?;
        if let Some(row) = row {
            if row.try_get::<String, _>("spec_yaml")? != machine.spec_yaml
                || row.try_get::<String, _>("current_state")? != machine.current_state
                || row.try_get::<bool, _>("ephemeral")? != machine.ephemeral
            {
                return Err(HandoverError::Conflict);
            }
        }
    }
    Ok(())
}

async fn import_slack_projects(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    capture: &Capture,
    apply: bool,
    already_active: bool,
) -> Result<(), HandoverError> {
    for project in &capture.slack_projects {
        if apply {
            sqlx::query("INSERT INTO project_slack_handover (project_id,ownership_epoch,capture_id,client_view_disposition) VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING")
            .bind(project.project_id).bind(capture.ownership_epoch).bind(capture.capture_id)
            .bind(project.client_view.as_str()).execute(&mut **transaction).await?;
        }
        let row = sqlx::query("SELECT ownership_epoch,capture_id,client_view_disposition,native_active FROM project_slack_handover WHERE project_id=$1")
            .bind(project.project_id).fetch_optional(&mut **transaction).await?;
        if let Some(row) = row {
            if row.try_get::<Uuid, _>("ownership_epoch")? != capture.ownership_epoch
                || row.try_get::<Option<Uuid>, _>("capture_id")? != Some(capture.capture_id)
                || row.try_get::<String, _>("client_view_disposition")?
                    != project.client_view.as_str()
                || row.try_get::<bool, _>("native_active")? != already_active
            {
                return Err(HandoverError::Conflict);
            }
        }
        let mut expected = project
            .main_push_seen
            .iter()
            .map(|key| ("main_push".to_owned(), key.clone()))
            .collect::<BTreeSet<_>>();
        if project.client_view == ClientViewDisposition::Posted {
            expected.insert(("client_view".to_owned(), "client-view".to_owned()));
        }
        if apply {
            for (kind, key) in &expected {
                sqlx::query("INSERT INTO project_slack_delivery (project_id,event_kind,event_key) VALUES ($1,$2,$3) ON CONFLICT DO NOTHING")
                .bind(project.project_id).bind(kind).bind(key).execute(&mut **transaction).await?;
            }
        }
        let actual: Vec<(String, String)> = sqlx::query_as(
            "SELECT event_kind,event_key FROM project_slack_delivery WHERE project_id=$1",
        )
        .bind(project.project_id)
        .fetch_all(&mut **transaction)
        .await?;
        let actual = actual.into_iter().collect::<BTreeSet<_>>();
        if (apply && actual != expected) || (!apply && !actual.is_subset(&expected)) {
            return Err(HandoverError::Conflict);
        }
    }
    Ok(())
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
        return Err(HandoverError::InvalidCapture);
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

/// Activate only after fresh authenticated source verification while the
/// operator continues holding all old writers. A hash match is insufficient.
/// The callback must perform the actual source readback, not inspect a file flag.
///
/// # Errors
/// Incomplete inventories, target drift, conflicting epochs or source verification fail closed.
pub async fn activate<F, Fut>(
    postgres: &PostgresDb,
    capture: &Capture,
    verify_source: F,
) -> Result<(), HandoverError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), HandoverError>>,
{
    capture.validate()?;
    let mut transaction = postgres.pool().begin().await?;
    sqlx::query("SET LOCAL lock_timeout='5s'")
        .execute(&mut *transaction)
        .await?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(OWNERSHIP_LOCK)
        .execute(&mut *transaction)
        .await?;
    let already_active = require_same_transfer(&mut transaction, capture.capture_id).await?;
    let inventory: Vec<Uuid> = sqlx::query_scalar("SELECT id FROM project ORDER BY id")
        .fetch_all(&mut *transaction)
        .await?;
    if inventory.into_iter().collect::<BTreeSet<_>>()
        != capture
            .slack_projects
            .iter()
            .map(|row| row.project_id)
            .collect()
    {
        return Err(HandoverError::Conflict);
    }
    let mut locks = capture
        .notation_machines
        .iter()
        .map(|row| crate::notation_machine_state::lane_key(row.notation_id))
        .collect::<BTreeSet<_>>();
    locks.extend(
        capture
            .slack_projects
            .iter()
            .map(|row| crate::project_slack_deliveries::lane_key(row.project_id)),
    );
    for lock in locks {
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(lock)
            .execute(&mut *transaction)
            .await?;
    }
    verify_notation_inventory(&mut transaction, capture).await?;
    verify_source().await?;
    verify_notation_inventory(&mut transaction, capture).await?;
    let digest = hex::encode(Sha256::digest(
        serde_json::to_vec(capture).map_err(|_| HandoverError::InvalidCapture)?,
    ));
    let imported: Option<String> = sqlx::query_scalar(
        "SELECT capture_sha256 FROM durable_object_handover WHERE capture_id=$1",
    )
    .bind(capture.capture_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if imported.as_deref() != Some(digest.as_str()) {
        return Err(HandoverError::Conflict);
    }
    verify_imported_rows(&mut transaction, capture, already_active).await?;
    sqlx::query("INSERT INTO durable_object_ownership (singleton,ownership_epoch,capture_id) VALUES (true,$1,$2) ON CONFLICT DO NOTHING")
        .bind(capture.ownership_epoch).bind(capture.capture_id).execute(&mut *transaction).await?;
    let epoch: Uuid =
        sqlx::query_scalar("SELECT ownership_epoch FROM durable_object_ownership WHERE singleton")
            .fetch_one(&mut *transaction)
            .await?;
    if epoch != capture.ownership_epoch {
        return Err(HandoverError::Conflict);
    }
    sqlx::query("UPDATE project_slack_handover SET native_active=true WHERE ownership_epoch=$1 AND capture_id=$2")
        .bind(capture.ownership_epoch).bind(capture.capture_id).execute(&mut *transaction).await?;
    transaction.commit().await?;
    Ok(())
}

async fn verify_imported_rows(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    capture: &Capture,
    already_active: bool,
) -> Result<(), HandoverError> {
    let notations = capture
        .notation_machines
        .iter()
        .map(|machine| machine.notation_id)
        .collect::<BTreeSet<_>>();
    for notation in notations {
        let pending:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notation_transition WHERE notation_id=$1 AND result IS NULL)")
            .bind(notation).fetch_one(&mut **transaction).await?;
        if pending {
            return Err(HandoverError::PendingNativeOperation);
        }
    }
    for machine in &capture.notation_machines {
        if !machine.ephemeral {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notation WHERE id=$1)")
                    .bind(machine.notation_id)
                    .fetch_one(&mut **transaction)
                    .await?;
            if !exists {
                return Err(HandoverError::InvalidCapture);
            }
        }
        let row=sqlx::query("SELECT spec_yaml,current_state,ephemeral FROM notation_machine_state WHERE notation_id=$1 AND machine_kind=$2")
            .bind(machine.notation_id).bind(&machine.machine_kind).fetch_optional(&mut **transaction).await?.ok_or(HandoverError::Conflict)?;
        if row.try_get::<String, _>("spec_yaml")? != machine.spec_yaml
            || row.try_get::<String, _>("current_state")? != machine.current_state
            || row.try_get::<bool, _>("ephemeral")? != machine.ephemeral
        {
            return Err(HandoverError::Conflict);
        }
    }
    for project in &capture.slack_projects {
        let row=sqlx::query("SELECT ownership_epoch,capture_id,client_view_disposition,native_active FROM project_slack_handover WHERE project_id=$1")
            .bind(project.project_id).fetch_optional(&mut **transaction).await?.ok_or(HandoverError::Conflict)?;
        if row.try_get::<Uuid, _>("ownership_epoch")? != capture.ownership_epoch
            || row.try_get::<Option<Uuid>, _>("capture_id")? != Some(capture.capture_id)
            || row.try_get::<String, _>("client_view_disposition")? != project.client_view.as_str()
            || row.try_get::<bool, _>("native_active")? != already_active
        {
            return Err(HandoverError::Conflict);
        }
        let mut expected = project
            .main_push_seen
            .iter()
            .map(|key| ("main_push".to_owned(), key.clone()))
            .collect::<BTreeSet<_>>();
        if project.client_view == ClientViewDisposition::Posted {
            expected.insert(("client_view".to_owned(), "client-view".to_owned()));
        }
        let actual: Vec<(String, String)> = sqlx::query_as(
            "SELECT event_kind,event_key FROM project_slack_delivery WHERE project_id=$1",
        )
        .bind(project.project_id)
        .fetch_all(&mut **transaction)
        .await?;
        if actual.into_iter().collect::<BTreeSet<_>>() != expected {
            return Err(HandoverError::Conflict);
        }
    }
    Ok(())
}

async fn verify_notation_inventory(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    capture: &Capture,
) -> Result<(), HandoverError> {
    let pending: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM notation_transition WHERE result IS NULL)")
            .fetch_one(&mut **transaction)
            .await?;
    if pending {
        return Err(HandoverError::PendingNativeOperation);
    }
    let actual: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT notation_id,machine_kind FROM notation_machine_state")
            .fetch_all(&mut **transaction)
            .await?;
    let expected = capture
        .notation_machines
        .iter()
        .map(|machine| (machine.notation_id, machine.machine_kind.clone()))
        .collect::<BTreeSet<_>>();
    if actual.into_iter().collect::<BTreeSet<_>>() != expected {
        return Err(HandoverError::Conflict);
    }
    Ok(())
}

async fn require_same_transfer(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    capture_id: Uuid,
) -> Result<bool, HandoverError> {
    let active=sqlx::query("SELECT o.capture_id,h.origin FROM durable_object_ownership o JOIN durable_object_handover h ON h.capture_id=o.capture_id WHERE o.singleton")
        .fetch_optional(&mut **transaction).await?;
    if let Some(active) = active {
        if active.try_get::<Uuid, _>("capture_id")? != capture_id
            || active.try_get::<String, _>("origin")? != "captured_source"
        {
            return Err(HandoverError::Conflict);
        }
        return Ok(true);
    }
    Ok(false)
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
