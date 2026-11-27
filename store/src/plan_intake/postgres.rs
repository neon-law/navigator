//! The PostgreSQL `plan_intake` table, the staged-assignment queue's system
//! of record whenever PostgreSQL is configured. Staging is idempotent on the
//! unique `source_message_id`; a decision applies only to a row still
//! `staged`, so two racing decisions cannot both land.

use sqlx::{postgres::PgRow, Connection, PgConnection, Row};
use uuid::Uuid;

use super::{PlanIntakeError, StagedAssignment, Status, TABLE};
use crate::postgres::PostgresDb;

const SELECT: &str = concat!(
    "plan_case_number, notice_matter_type, take_started_at, ",
    "id, firm_id, conversation_id, source_message_id, provider, member_name, ",
    "member_email, member_person_id, member_entity_id, plan_member_id, benefit_code, plan_number, ",
    "plan_matter_type, plan_state, assigned_at, legal_plan_id, legal_plan_name, plan_portal_only, ",
    "fee_covered_benefit, fee_plan_fee_cents, fee_supplemental_flat_cents, ",
    "fee_supplemental_hourly_cents, fee_effective_on, conflict_summary, conflict_blocking, ",
    "conflict_checked, status, project_id, accepted_by, decided_at, decline_reason, inserted_at, updated_at"
);

fn staged(row: &PgRow) -> Result<StagedAssignment, sqlx::Error> {
    let summary: String = row.try_get("conflict_summary")?;
    Ok(StagedAssignment {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        conversation_id: row.try_get("conversation_id")?,
        source_message_id: row.try_get("source_message_id")?,
        provider: row.try_get("provider")?,
        member_name: row.try_get("member_name")?,
        member_email: row.try_get("member_email")?,
        member_person_id: row.try_get("member_person_id")?,
        member_entity_id: row.try_get("member_entity_id")?,
        plan_member_id: row.try_get("plan_member_id")?,
        benefit_code: row.try_get("benefit_code")?,
        plan_number: row.try_get("plan_number")?,
        plan_case_number: row.try_get("plan_case_number")?,
        notice_matter_type: row.try_get("notice_matter_type")?,
        plan_matter_type: row.try_get("plan_matter_type")?,
        plan_state: row.try_get("plan_state")?,
        assigned_at: row.try_get("assigned_at")?,
        legal_plan_id: row.try_get("legal_plan_id")?,
        legal_plan_name: row.try_get("legal_plan_name")?,
        plan_portal_only: row.try_get("plan_portal_only")?,
        fee_covered_benefit: row.try_get("fee_covered_benefit")?,
        fee_plan_fee_cents: row.try_get("fee_plan_fee_cents")?,
        fee_supplemental_flat_cents: row.try_get("fee_supplemental_flat_cents")?,
        fee_supplemental_hourly_cents: row.try_get("fee_supplemental_hourly_cents")?,
        fee_effective_on: row.try_get("fee_effective_on")?,
        conflict_summary: summary
            .lines()
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        conflict_blocking: row.try_get("conflict_blocking")?,
        conflict_checked: row.try_get("conflict_checked")?,
        status: row.try_get("status")?,
        project_id: row.try_get("project_id")?,
        accepted_by: row.try_get("accepted_by")?,
        decided_at: row.try_get("decided_at")?,
        take_started_at: row.try_get("take_started_at")?,
        decline_reason: row.try_get("decline_reason")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Insert a row as given, keeping its id; `None` when its source message is
/// already staged (or the id already exists).
async fn insert(
    pg: &PostgresDb,
    row: &StagedAssignment,
) -> Result<Option<StagedAssignment>, sqlx::Error> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, firm_id, conversation_id, source_message_id, provider, \
         member_name, member_email, member_person_id, member_entity_id, plan_member_id, \
         benefit_code, plan_number, plan_matter_type, plan_state, assigned_at, legal_plan_id, \
         legal_plan_name, plan_portal_only, fee_covered_benefit, fee_plan_fee_cents, \
         fee_supplemental_flat_cents, fee_supplemental_hourly_cents, fee_effective_on, \
         conflict_summary, conflict_blocking, conflict_checked, status, project_id, \
         accepted_by, decided_at, decline_reason, inserted_at, updated_at, \
         plan_case_number, notice_matter_type) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, \
         $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31, $32, $33, $34, $35) \
         ON CONFLICT DO NOTHING RETURNING {SELECT}"
    )))
    .bind(row.id)
    .bind(row.firm_id)
    .bind(row.conversation_id)
    .bind(row.source_message_id)
    .bind(&row.provider)
    .bind(&row.member_name)
    .bind(&row.member_email)
    .bind(row.member_person_id)
    .bind(row.member_entity_id)
    .bind(&row.plan_member_id)
    .bind(&row.benefit_code)
    .bind(&row.plan_number)
    .bind(&row.plan_matter_type)
    .bind(&row.plan_state)
    .bind(row.assigned_at)
    .bind(row.legal_plan_id)
    .bind(&row.legal_plan_name)
    .bind(row.plan_portal_only)
    .bind(&row.fee_covered_benefit)
    .bind(row.fee_plan_fee_cents)
    .bind(row.fee_supplemental_flat_cents)
    .bind(row.fee_supplemental_hourly_cents)
    .bind(row.fee_effective_on)
    .bind(row.conflict_summary.join("\n"))
    .bind(row.conflict_blocking)
    .bind(row.conflict_checked)
    .bind(&row.status)
    .bind(row.project_id)
    .bind(row.accepted_by)
    .bind(row.decided_at)
    .bind(&row.decline_reason)
    .bind(row.inserted_at)
    .bind(row.updated_at)
    .bind(&row.plan_case_number)
    .bind(&row.notice_matter_type)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(staged)
    .transpose()
}

/// Stage a new row. A redelivered notice whose message another staging
/// already recorded returns that row.
pub(crate) async fn stage(
    pg: &PostgresDb,
    row: &StagedAssignment,
) -> Result<StagedAssignment, PlanIntakeError> {
    if let Some(inserted) = insert(pg, row).await? {
        return Ok(inserted);
    }
    find_by_source_message(pg, row.source_message_id)
        .await?
        .ok_or(PlanIntakeError::WriteReturnedNothing)
}

// Keep locks on a dedicated connection so waiting decisions cannot exhaust
// the pool the domain writes use. Disconnect releases the locks, even on error.
pub(super) async fn lock(pg: &PostgresDb, key: &str) -> Result<PgConnection, PlanIntakeError> {
    let mut connection = PgConnection::connect_with(&pg.pool().connect_options()).await?;
    lock_in(&mut connection, key).await?;
    Ok(connection)
}

pub(super) async fn lock_in(
    connection: &mut PgConnection,
    key: &str,
) -> Result<(), PlanIntakeError> {
    sqlx::query("SELECT pg_advisory_lock(hashtextextended($1, 0))")
        .bind(key)
        .execute(connection)
        .await?;
    Ok(())
}

pub(super) async fn save_member(
    pg: &PostgresDb,
    row: &StagedAssignment,
) -> Result<(), PlanIntakeError> {
    sqlx::query(
        "UPDATE plan_intake SET member_name=$2, member_email=$3, member_person_id=$4,
         member_entity_id=$5, conflict_summary=$6, conflict_blocking=$7, conflict_checked=$8,
         updated_at=now() WHERE id=$1 AND status='staged'",
    )
    .bind(row.id)
    .bind(&row.member_name)
    .bind(&row.member_email)
    .bind(row.member_person_id)
    .bind(row.member_entity_id)
    .bind(row.conflict_summary.join("\n"))
    .bind(row.conflict_blocking)
    .bind(row.conflict_checked)
    .execute(pg.pool())
    .await?;
    Ok(())
}

pub(super) async fn reserve_case(
    pg: &PostgresDb,
    row: &StagedAssignment,
    plan: Uuid,
    case_number: &str,
    started_at: chrono::DateTime<chrono::Utc>,
) -> Result<(), PlanIntakeError> {
    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM plan_intake WHERE firm_id=$1 AND provider=$2
             AND lower(plan_case_number)=lower($3) AND id<>$4
             AND (status='accepted' OR (status='staged' AND take_started_at IS NOT NULL))
         UNION ALL SELECT 1 FROM project WHERE firm_id=$1 AND legal_plan_id=$5
             AND lower(plan_case_number)=lower($3) AND id<>$4)",
    )
    .bind(row.firm_id)
    .bind(&row.provider)
    .bind(case_number)
    .bind(row.id)
    .bind(plan)
    .fetch_one(pg.pool())
    .await?;
    if duplicate {
        return Err(PlanIntakeError::DuplicateCase);
    }
    sqlx::query(
        "UPDATE plan_intake SET plan_case_number=$2, take_started_at=$3
         WHERE id=$1 AND status='staged'",
    )
    .bind(row.id)
    .bind(case_number)
    .bind(started_at)
    .execute(pg.pool())
    .await?;
    Ok(())
}

pub(super) async fn release_case(pg: &PostgresDb, id: Uuid) -> Result<(), PlanIntakeError> {
    sqlx::query("UPDATE plan_intake SET take_started_at=NULL WHERE id=$1 AND status='staged'")
        .bind(id)
        .execute(pg.pool())
        .await?;
    Ok(())
}

/// Record a decision: every column [`super::accept`] or [`super::decline`]
/// sets, applied only while the row is still staged.
pub(super) async fn decide(
    pg: &PostgresDb,
    row: &StagedAssignment,
) -> Result<StagedAssignment, PlanIntakeError> {
    let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
        "UPDATE {TABLE} SET status = $2, project_id = $3, accepted_by = $4, decided_at = $5, \
         decline_reason = $6, plan_matter_type = $7, plan_state = $8, legal_plan_id = $9, \
         legal_plan_name = $10, plan_portal_only = $11, fee_covered_benefit = $12, \
         fee_plan_fee_cents = $13, fee_supplemental_flat_cents = $14, \
         fee_supplemental_hourly_cents = $15, fee_effective_on = $16, updated_at = $17, \
         take_started_at = NULL \
         WHERE id = $1 AND status = $18 AND ($2 <> 'declined' OR take_started_at IS NULL) RETURNING {SELECT}"
    )))
    .bind(row.id)
    .bind(&row.status)
    .bind(row.project_id)
    .bind(row.accepted_by)
    .bind(row.decided_at)
    .bind(&row.decline_reason)
    .bind(&row.plan_matter_type)
    .bind(&row.plan_state)
    .bind(row.legal_plan_id)
    .bind(&row.legal_plan_name)
    .bind(row.plan_portal_only)
    .bind(&row.fee_covered_benefit)
    .bind(row.fee_plan_fee_cents)
    .bind(row.fee_supplemental_flat_cents)
    .bind(row.fee_supplemental_hourly_cents)
    .bind(row.fee_effective_on)
    .bind(row.updated_at)
    .bind(Status::Staged.as_str())
    .fetch_optional(&pg.0)
    .await?;
    updated
        .as_ref()
        .map(staged)
        .transpose()?
        .ok_or(PlanIntakeError::NotStaged)
}

/// See [`super::find_by_id`].
pub(super) async fn find_by_id(
    pg: &PostgresDb,
    id: Uuid,
) -> Result<Option<StagedAssignment>, PlanIntakeError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE id = $1"
    )))
    .bind(id)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(staged)
    .transpose()?)
}

/// See [`super::find_by_source_message`].
pub(super) async fn find_by_source_message(
    pg: &PostgresDb,
    source_message_id: Uuid,
) -> Result<Option<StagedAssignment>, PlanIntakeError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE source_message_id = $1"
    )))
    .bind(source_message_id)
    .fetch_optional(&pg.0)
    .await?
    .as_ref()
    .map(staged)
    .transpose()?)
}

/// See [`super::list_by_firm`].
pub(super) async fn list_by_firm(
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<Vec<StagedAssignment>, PlanIntakeError> {
    Ok(sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE firm_id = $1 ORDER BY assigned_at DESC, id DESC \
         LIMIT 100"
    )))
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(staged)
    .collect::<Result<_, _>>()?)
}
