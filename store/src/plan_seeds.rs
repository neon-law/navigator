//! PostgreSQL-native Plan-panel seeds (ENG-1269): the six canonical Case
//! Stages, the anchor Firm's two sample Legal Plans, and their round fee
//! schedules, applied from the YAML inventory under `store/seeds/` — the
//! same files the seed-boundary and no-client-data gates scan.
//!
//! The SurrealDB seed loader covers SurrealDB-authored tables only; every
//! table here is PostgreSQL-native, so this module applies its YAML at web
//! boot, after the anchor Firm is seeded in SurrealDB and mirrored into
//! PostgreSQL. Every insert is idempotent on its natural key and never
//! overwrites a row the Firm has since changed: seeds converge a fresh
//! deployment, they are not configuration.

use chrono::NaiveDate;
use serde_yaml::Value;
use uuid::Uuid;

use crate::firms::FirmError;
use crate::postgres::PostgresDb;
use crate::postgres_mirror::{self, MirrorError, MirrorKey};
use crate::surreal::SurrealDb;

const LEGAL_PLAN_YAML: &str = include_str!("../seeds/LegalPlan.yaml");
const PLAN_FEE_SCHEDULE_YAML: &str = include_str!("../seeds/PlanFeeSchedule.yaml");
const STAGE_DEFINITION_YAML: &str = include_str!("../seeds/StageDefinition.yaml");

/// Why a Plan-panel seed run failed. Carries no row values.
#[derive(Debug, thiserror::Error)]
pub enum PlanSeedError {
    #[error("the seed YAML is malformed: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error(transparent)]
    Firm(#[from] FirmError),
    #[error("PostgreSQL is unavailable for the Plan-panel seed")]
    PostgresUnavailable,
    #[error("the anchor Firm row could not be mirrored into PostgreSQL")]
    Mirror(#[from] MirrorError),
    #[error("PostgreSQL refused the seed row")]
    Postgres(#[from] sqlx::Error),
}

/// Seeds the Plan-panel tables for the deployment's anchor Firm. A no-op
/// when the PostgreSQL mirror is not configured (there is nothing to seed
/// into), when no anchor Firm exists yet, or when the Firm row cannot be
/// mirrored — the next boot retries, and every insert is idempotent.
///
/// The Firm row is mirrored explicitly first so the seed rows' foreign keys
/// resolve on the boot that first creates the Firm, before any other write
/// has carried it over.
///
/// # Errors
///
/// [`PlanSeedError`] when the seed YAML is malformed, the anchor Firm
/// cannot be read, or PostgreSQL refuses a seed row.
pub async fn seed_quickstart(surreal: &SurrealDb) -> Result<(), PlanSeedError> {
    if !postgres_mirror::enabled() {
        return Ok(());
    }
    let Some(firm) = crate::firms::anchor_firm(surreal).await? else {
        return Ok(());
    };
    let pg = PostgresDb::from_env()
        .await
        .map_err(|_| PlanSeedError::PostgresUnavailable)?;
    postgres_mirror::mirror(surreal, &pg, &MirrorKey::Firm(firm.id)).await?;
    seed_stage_definitions(&pg, firm.id).await?;
    let plans = seed_legal_plans(&pg, firm.id).await?;
    seed_fee_schedules(&pg, firm.id, &plans).await?;
    Ok(())
}

/// YAML rows as generic maps; a scalar where an object belongs is a seed
/// authoring error the caller reports.
fn rows(yaml: &str) -> Result<Vec<Value>, PlanSeedError> {
    let doc: Value = serde_yaml::from_str(yaml)?;
    let records = doc
        .get("records")
        .and_then(Value::as_sequence)
        .cloned()
        .unwrap_or_default();
    Ok(records)
}

fn text<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

fn integer(row: &Value, key: &str) -> Option<i64> {
    row.get(key).and_then(Value::as_i64)
}

/// Seeds the six canonical stages for the Firm, keyed on the Firm's own
/// `(firm_id, position)` uniqueness so a re-seed never touches a stage the
/// Firm has renamed or reordered.
async fn seed_stage_definitions(pg: &PostgresDb, firm_id: Uuid) -> Result<(), PlanSeedError> {
    for row in rows(STAGE_DEFINITION_YAML)? {
        let Some(label) = text(&row, "label").map(str::to_owned) else {
            continue;
        };
        let Some(position) = integer(&row, "position").and_then(|value| i32::try_from(value).ok())
        else {
            continue;
        };
        sqlx::query(
            "INSERT INTO stage_definition (id,firm_id,label,position,inserted_at,updated_at) \
             VALUES (gen_random_uuid(),$1,$2,$3,now(),now()) \
             ON CONFLICT (firm_id, position) DO NOTHING",
        )
        .bind(firm_id)
        .bind(label)
        .bind(position)
        .execute(&pg.0)
        .await?;
    }
    Ok(())
}

/// Seeds the sample Legal Plans and returns `(name, id)` so the fee-schedule
/// seed can resolve its plan references. The no-op update keeps a re-seed
/// returning the existing row's id without rewriting the Firm's plan.
async fn seed_legal_plans(
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<Vec<(String, Uuid)>, PlanSeedError> {
    let mut plans = Vec::new();
    for row in rows(LEGAL_PLAN_YAML)? {
        let Some(name) = text(&row, "name").map(str::to_owned) else {
            continue;
        };
        let Some(acceptance_sla_hours) =
            integer(&row, "acceptance_sla_hours").and_then(|v| i32::try_from(v).ok())
        else {
            continue;
        };
        let Some(turnaround_sla_days) =
            integer(&row, "turnaround_sla_days").and_then(|v| i32::try_from(v).ok())
        else {
            continue;
        };
        let reporting_requirements = text(&row, "reporting_requirements");
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO legal_plan \
             (firm_id,name,acceptance_sla_hours,turnaround_sla_days,reporting_requirements) \
             VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (firm_id, name) DO UPDATE SET name = EXCLUDED.name \
             RETURNING id",
        )
        .bind(firm_id)
        .bind(&name)
        .bind(acceptance_sla_hours)
        .bind(turnaround_sla_days)
        .bind(reporting_requirements)
        .fetch_one(&pg.0)
        .await?;
        plans.push((name, id));
    }
    Ok(plans)
}

/// Seeds the sample fee schedules, resolving each row's plan by name. The
/// database's CHECK constraints and same-Firm trigger are the validators;
/// a seed row that violates them fails the boot rather than shipping.
async fn seed_fee_schedules(
    pg: &PostgresDb,
    firm_id: Uuid,
    plans: &[(String, Uuid)],
) -> Result<(), PlanSeedError> {
    for row in rows(PLAN_FEE_SCHEDULE_YAML)? {
        let Some(plan_name) = row.get("plan").and_then(|plan| text(plan, "name")) else {
            continue;
        };
        let Some(plan_id) = plans
            .iter()
            .find(|(name, _)| name == plan_name)
            .map(|(_, id)| *id)
        else {
            continue;
        };
        let Some(matter_type) = text(&row, "matter_type").map(str::to_owned) else {
            continue;
        };
        let Some(state) = text(&row, "state").map(str::to_owned) else {
            continue;
        };
        let Some(covered_benefit) = text(&row, "covered_benefit").map(str::to_owned) else {
            continue;
        };
        let Some(plan_fee_cents) =
            integer(&row, "plan_fee_cents").and_then(|v| i32::try_from(v).ok())
        else {
            continue;
        };
        let supplemental_hourly_cents =
            integer(&row, "supplemental_hourly_cents").and_then(|v| i32::try_from(v).ok());
        let Some(effective_on) =
            text(&row, "effective_on").and_then(|text| NaiveDate::parse_from_str(text, "%F").ok())
        else {
            continue;
        };
        sqlx::query(
            "INSERT INTO plan_fee_schedule \
             (id,firm_id,legal_plan_id,matter_type,state,covered_benefit,plan_fee_cents, \
             supplemental_hourly_cents,effective_on) \
             VALUES (gen_random_uuid(),$1,$2,$3,$4,$5,$6,$7,$8) \
             ON CONFLICT (firm_id, legal_plan_id, matter_type, state, effective_on) \
             DO NOTHING",
        )
        .bind(firm_id)
        .bind(plan_id)
        .bind(&matter_type)
        .bind(&state)
        .bind(&covered_benefit)
        .bind(plan_fee_cents)
        .bind(supplemental_hourly_cents)
        .bind(effective_on)
        .execute(&pg.0)
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{seed_fee_schedules, seed_legal_plans, seed_stage_definitions};
    use crate::postgres_mirror::tests::{connect, fresh_schema};
    use sqlx::Row;
    use uuid::Uuid;

    #[tokio::test]
    async fn seeds_converge_and_never_reprice_the_firm() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let firm_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO firm (id,name,slug,status) VALUES ($1,'Guieb & Shook PC',$2,'active')",
        )
        .bind(firm_id)
        .bind(format!("ps-{}", Uuid::new_v4().simple()))
        .execute(&pg.0)
        .await
        .unwrap();

        let plans = seed_legal_plans(&pg, firm_id).await.unwrap();
        seed_stage_definitions(&pg, firm_id).await.unwrap();
        seed_fee_schedules(&pg, firm_id, &plans).await.unwrap();
        let plan_rows = sqlx::query("SELECT count(*) AS n FROM legal_plan WHERE firm_id=$1")
            .bind(firm_id)
            .fetch_one(&pg.0)
            .await
            .unwrap()
            .get::<i64, _>("n");
        let fee_rows = sqlx::query("SELECT count(*) AS n FROM plan_fee_schedule WHERE firm_id=$1")
            .bind(firm_id)
            .fetch_one(&pg.0)
            .await
            .unwrap()
            .get::<i64, _>("n");
        let stage_rows = sqlx::query("SELECT count(*) AS n FROM stage_definition WHERE firm_id=$1")
            .bind(firm_id)
            .fetch_one(&pg.0)
            .await
            .unwrap()
            .get::<i64, _>("n");

        // A second boot over the same Firm is a no-op: the plans keep their
        // ids, the append-only fee history gains no rows, and the Firm's
        // stage vocabulary is untouched.
        let replay_plans = seed_legal_plans(&pg, firm_id).await.unwrap();
        seed_stage_definitions(&pg, firm_id).await.unwrap();
        seed_fee_schedules(&pg, firm_id, &replay_plans)
            .await
            .unwrap();
        assert_eq!(replay_plans, plans, "the plans keep their ids");
        assert_eq!(
            sqlx::query("SELECT count(*) AS n FROM plan_fee_schedule WHERE firm_id=$1")
                .bind(firm_id)
                .fetch_one(&pg.0)
                .await
                .unwrap()
                .get::<i64, _>("n"),
            fee_rows,
            "the append-only fee history gains no rows on re-seed"
        );
        assert_eq!(
            sqlx::query("SELECT count(*) AS n FROM legal_plan WHERE firm_id=$1")
                .bind(firm_id)
                .fetch_one(&pg.0)
                .await
                .unwrap()
                .get::<i64, _>("n"),
            plan_rows
        );
        assert_eq!(
            sqlx::query("SELECT count(*) AS n FROM stage_definition WHERE firm_id=$1")
                .bind(firm_id)
                .fetch_one(&pg.0)
                .await
                .unwrap()
                .get::<i64, _>("n"),
            stage_rows
        );
        assert_eq!(stage_rows, 6, "the six canonical stages are seeded");
    }
}
