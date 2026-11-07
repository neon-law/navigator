//! A Firm's Legal Plans and their effective-dated Plan Fee Schedules, stored
//! relationally in PostgreSQL.
//!
//! A Legal Plan is a panel program the Firm performs work for (ARAG,
//! LegalShield, …). The vocabulary is Firm data — two Firms on one deployment
//! each hold their own ARAG row with their own contact and service levels —
//! never shared reference data, the way [`stage_definition`](crate::case_stages)
//! is the Firm's own Case Stage vocabulary. Every plan row carries the
//! acceptance and turnaround service levels the reporting compares actuals
//! against.
//!
//! A Plan Fee Schedule row is what one plan pays for one covered benefit.
//! Like a [`ContingencyTerm`](crate::contingency_terms::ContingencyTerm), a
//! schedule is **dated and never edited**: a change is a new row with its own
//! `effective_on`, and the database rejects updates, deletes, and truncates,
//! so the table is its own history. Resolution takes the latest row effective
//! on or before the date asked — the attorney_rate rule. The database also
//! refuses a schedule row whose plan belongs to another Firm, the same
//! cross-Firm check a Contingency Term tier's trigger makes.
//!
//! Amounts are integer cents, never floats. `matter_type` is a closed
//! vocabulary shared with `project.plan_matter_type` and the assumption keys
//! the CEO digest reads. See `docs/glossary/legal-plan.md` and
//! `docs/glossary/plan-fee-schedule.md`.
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum LegalPlanError {
    #[error("invalid Legal Plan: {0}")]
    Invalid(&'static str),
    #[error("the plan fee schedule names a Legal Plan of another Firm")]
    PlanNotInFirm,
    #[error("Legal Plan or fee schedule was not found")]
    NotFound,
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// The kind of matter a plan assignment is. A closed vocabulary shared by
/// `project.plan_matter_type`, `plan_fee_schedule.matter_type`, and the
/// assumption keys; a value outside it never enters the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanMatterType {
    DocumentReview,
    Letter,
    UncontestedDivorce,
    CourtAppearance,
}

impl PlanMatterType {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DocumentReview => "document_review",
            Self::Letter => "letter",
            Self::UncontestedDivorce => "uncontested_divorce",
            Self::CourtAppearance => "court_appearance",
        }
    }

    /// Parses the closed vocabulary's stored word. The inverse of [`Self::as_str`].
    ///
    /// # Errors
    /// [`LegalPlanError::Invalid`] for any word outside the vocabulary.
    pub fn parse(raw: &str) -> Result<Self, LegalPlanError> {
        match raw {
            "document_review" => Ok(Self::DocumentReview),
            "letter" => Ok(Self::Letter),
            "uncontested_divorce" => Ok(Self::UncontestedDivorce),
            "court_appearance" => Ok(Self::CourtAppearance),
            _ => Err(LegalPlanError::Invalid("matter_type")),
        }
    }
}

/// One Legal Plan the Firm works.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalPlan {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub name: String,
    pub contact: Option<String>,
    pub portal: Option<String>,
    /// How long the Firm has to accept an assignment, in hours, from the
    /// moment the assignment email lands.
    pub acceptance_sla_hours: i32,
    /// How long the Firm has to finish the matter, in days, from acceptance.
    pub turnaround_sla_days: i32,
    /// What the plan asks the Firm to report and how often.
    pub reporting_requirements: Option<String>,
    pub inserted_at: DateTime<Utc>,
}

/// The inputs to [`create`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewLegalPlan {
    pub name: String,
    #[serde(default)]
    pub contact: Option<String>,
    #[serde(default)]
    pub portal: Option<String>,
    pub acceptance_sla_hours: i32,
    pub turnaround_sla_days: i32,
    #[serde(default)]
    pub reporting_requirements: Option<String>,
}

impl NewLegalPlan {
    fn validate(&self) -> Result<(), LegalPlanError> {
        if self.name.trim().is_empty() || self.name.len() > 200 {
            return Err(LegalPlanError::Invalid("name"));
        }
        if self.acceptance_sla_hours <= 0 {
            return Err(LegalPlanError::Invalid("acceptance_sla_hours"));
        }
        if self.turnaround_sla_days <= 0 {
            return Err(LegalPlanError::Invalid("turnaround_sla_days"));
        }
        Ok(())
    }
}

/// Create a Legal Plan in the Firm's vocabulary. A plan name is unique per
/// Firm; another Firm may hold its own row for the same program.
///
/// # Errors
///
/// [`LegalPlanError`] for invalid input or a database failure.
pub async fn create(
    pg: &PostgresDb,
    firm_id: Uuid,
    input: &NewLegalPlan,
) -> Result<LegalPlan, LegalPlanError> {
    input.validate()?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO legal_plan \
         (firm_id,name,contact,portal,acceptance_sla_hours,turnaround_sla_days,reporting_requirements) \
         VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id",
    )
    .bind(firm_id)
    .bind(input.name.trim())
    .bind(input.contact.as_deref().map(str::trim))
    .bind(input.portal.as_deref().map(str::trim))
    .bind(input.acceptance_sla_hours)
    .bind(input.turnaround_sla_days)
    .bind(input.reporting_requirements.as_deref().map(str::trim))
    .fetch_one(&pg.0)
    .await?;
    find(pg, firm_id, id).await?.ok_or(LegalPlanError::NotFound)
}

/// The Firm's plans, name order.
///
/// # Errors
///
/// [`LegalPlanError::Database`] on a database failure.
pub async fn list(pg: &PostgresDb, firm_id: Uuid) -> Result<Vec<LegalPlan>, LegalPlanError> {
    sqlx::query(
        "SELECT id,firm_id,name,contact,portal,acceptance_sla_hours,turnaround_sla_days, \
         reporting_requirements,inserted_at FROM legal_plan WHERE firm_id=$1 ORDER BY name, id",
    )
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(plan_from_row)
    .collect()
}

/// One plan, readable only inside the Firm that holds it.
///
/// # Errors
///
/// [`LegalPlanError::NotFound`] for a plan of another Firm, or a database
/// failure.
pub async fn find(
    pg: &PostgresDb,
    firm_id: Uuid,
    plan_id: Uuid,
) -> Result<Option<LegalPlan>, LegalPlanError> {
    sqlx::query(
        "SELECT id,firm_id,name,contact,portal,acceptance_sla_hours,turnaround_sla_days, \
         reporting_requirements,inserted_at FROM legal_plan WHERE firm_id=$1 AND id=$2",
    )
    .bind(firm_id)
    .bind(plan_id)
    .fetch_optional(&pg.0)
    .await?
    .map(|row| plan_from_row(&row))
    .transpose()
}

/// Which Firm holds one plan, by id alone. The check the Project plan
/// columns make when they name a plan: the column must point into the
/// Project's own Firm ([`super::projects`]`::set_plan_columns`), and this
/// lookup only answers "does the plan exist, and whose is it" — Firm-scoped
/// reads stay with [`find`].
///
/// # Errors
///
/// [`LegalPlanError::Database`] on a database failure.
pub async fn firm_of(pg: &PostgresDb, plan_id: Uuid) -> Result<Option<Uuid>, LegalPlanError> {
    Ok(
        sqlx::query_scalar("SELECT firm_id FROM legal_plan WHERE id=$1")
            .bind(plan_id)
            .fetch_optional(&pg.0)
            .await?,
    )
}

fn plan_from_row(row: &sqlx::postgres::PgRow) -> Result<LegalPlan, LegalPlanError> {
    Ok(LegalPlan {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        name: row.try_get("name")?,
        contact: row.try_get("contact")?,
        portal: row.try_get("portal")?,
        acceptance_sla_hours: row.try_get("acceptance_sla_hours")?,
        turnaround_sla_days: row.try_get("turnaround_sla_days")?,
        reporting_requirements: row.try_get("reporting_requirements")?,
        inserted_at: row.try_get("inserted_at")?,
    })
}

/// One effective-dated Plan Fee Schedule row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFeeSchedule {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub legal_plan_id: Uuid,
    pub matter_type: PlanMatterType,
    /// A seeded jurisdiction code ('ny', 'nv', 'ca', 'wa'), the shape
    /// `onboarding_jurisdiction` stores.
    pub state: String,
    pub covered_benefit: String,
    /// What the plan pays for the covered benefit, in integer cents.
    pub plan_fee_cents: i32,
    /// The hourly amount billed beyond the benefit, in integer cents.
    pub supplemental_hourly_cents: Option<i32>,
    /// A flat amount billed beyond the benefit, in integer cents.
    pub supplemental_flat_cents: Option<i32>,
    pub effective_on: NaiveDate,
    pub inserted_at: DateTime<Utc>,
}

/// The inputs to [`add_fee_schedule`].
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewPlanFeeSchedule {
    pub firm_id: Uuid,
    pub legal_plan_id: Uuid,
    pub matter_type: PlanMatterType,
    pub state: String,
    pub covered_benefit: String,
    pub plan_fee_cents: i32,
    #[serde(default)]
    pub supplemental_hourly_cents: Option<i32>,
    #[serde(default)]
    pub supplemental_flat_cents: Option<i32>,
    pub effective_on: NaiveDate,
}

impl NewPlanFeeSchedule {
    fn validate(&self) -> Result<(), LegalPlanError> {
        if !valid_state_code(&self.state) {
            return Err(LegalPlanError::Invalid("state"));
        }
        if self.covered_benefit.trim().is_empty() || self.covered_benefit.len() > 500 {
            return Err(LegalPlanError::Invalid("covered_benefit"));
        }
        if self.plan_fee_cents < 0 {
            return Err(LegalPlanError::Invalid("plan_fee_cents"));
        }
        if self
            .supplemental_hourly_cents
            .is_some_and(|cents| cents < 0)
        {
            return Err(LegalPlanError::Invalid("supplemental_hourly_cents"));
        }
        if self.supplemental_flat_cents.is_some_and(|cents| cents < 0) {
            return Err(LegalPlanError::Invalid("supplemental_flat_cents"));
        }
        Ok(())
    }
}

/// A jurisdiction code is lowercase ASCII letters — 'ny', not 'NY'.
pub fn valid_state_code(state: &str) -> bool {
    (2..=16).contains(&state.len()) && state.bytes().all(|byte| byte.is_ascii_lowercase())
}

/// Record a new effective-dated fee for one covered benefit. Never replaces a
/// row: a change is a new `effective_on`, and the database rejects edits and
/// removals. The schedule row and its plan must belong to the same Firm —
/// checked here and enforced again by the database for any writer that skips
/// this accessor.
///
/// # Errors
///
/// [`LegalPlanError`] for invalid input, a plan of another Firm, or a
/// database failure.
pub async fn add_fee_schedule(
    pg: &PostgresDb,
    input: &NewPlanFeeSchedule,
) -> Result<PlanFeeSchedule, LegalPlanError> {
    input.validate()?;
    let in_firm: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM legal_plan WHERE id=$1 AND firm_id=$2)")
            .bind(input.legal_plan_id)
            .bind(input.firm_id)
            .fetch_one(&pg.0)
            .await?;
    if !in_firm {
        return Err(LegalPlanError::PlanNotInFirm);
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO plan_fee_schedule \
         (firm_id,legal_plan_id,matter_type,state,covered_benefit,plan_fee_cents, \
          supplemental_hourly_cents,supplemental_flat_cents,effective_on) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id",
    )
    .bind(input.firm_id)
    .bind(input.legal_plan_id)
    .bind(input.matter_type.as_str())
    .bind(&input.state)
    .bind(input.covered_benefit.trim())
    .bind(input.plan_fee_cents)
    .bind(input.supplemental_hourly_cents)
    .bind(input.supplemental_flat_cents)
    .bind(input.effective_on)
    .fetch_one(&pg.0)
    .await?;
    fee_schedule(pg, input.firm_id, id)
        .await?
        .ok_or(LegalPlanError::NotFound)
}

/// The plan's fee history for one matter type and state, oldest effective
/// date first.
///
/// # Errors
///
/// [`LegalPlanError::Database`] on a database failure.
pub async fn fee_history(
    pg: &PostgresDb,
    firm_id: Uuid,
    legal_plan_id: Uuid,
    matter_type: PlanMatterType,
    state: &str,
) -> Result<Vec<PlanFeeSchedule>, LegalPlanError> {
    if !valid_state_code(state) {
        return Err(LegalPlanError::Invalid("state"));
    }
    sqlx::query(
        "SELECT id,firm_id,legal_plan_id,matter_type,state,covered_benefit,plan_fee_cents, \
         supplemental_hourly_cents,supplemental_flat_cents,effective_on,inserted_at \
         FROM plan_fee_schedule WHERE firm_id=$1 AND legal_plan_id=$2 AND matter_type=$3 \
         AND state=$4 ORDER BY effective_on, inserted_at, id",
    )
    .bind(firm_id)
    .bind(legal_plan_id)
    .bind(matter_type.as_str())
    .bind(state)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(schedule_from_row)
    .collect()
}

/// The fee that governs one covered benefit on `on`: the latest row
/// effective on or before that date, or `None` when the plan prices no such
/// benefit yet. Reads stay inside the Firm's own rows.
///
/// # Errors
///
/// [`LegalPlanError`] for an invalid state code or a database failure.
pub async fn resolve_fee(
    pg: &PostgresDb,
    firm_id: Uuid,
    legal_plan_id: Uuid,
    matter_type: PlanMatterType,
    state: &str,
    on: NaiveDate,
) -> Result<Option<PlanFeeSchedule>, LegalPlanError> {
    if !valid_state_code(state) {
        return Err(LegalPlanError::Invalid("state"));
    }
    sqlx::query(
        "SELECT id,firm_id,legal_plan_id,matter_type,state,covered_benefit,plan_fee_cents, \
         supplemental_hourly_cents,supplemental_flat_cents,effective_on,inserted_at \
         FROM plan_fee_schedule WHERE firm_id=$1 AND legal_plan_id=$2 AND matter_type=$3 \
         AND state=$4 AND effective_on<=$5 \
         ORDER BY effective_on DESC, inserted_at DESC, id DESC LIMIT 1",
    )
    .bind(firm_id)
    .bind(legal_plan_id)
    .bind(matter_type.as_str())
    .bind(state)
    .bind(on)
    .fetch_optional(&pg.0)
    .await?
    .map(|row| schedule_from_row(&row))
    .transpose()
}

async fn fee_schedule(
    pg: &PostgresDb,
    firm_id: Uuid,
    schedule_id: Uuid,
) -> Result<Option<PlanFeeSchedule>, LegalPlanError> {
    sqlx::query(
        "SELECT id,firm_id,legal_plan_id,matter_type,state,covered_benefit,plan_fee_cents, \
         supplemental_hourly_cents,supplemental_flat_cents,effective_on,inserted_at \
         FROM plan_fee_schedule WHERE firm_id=$1 AND id=$2",
    )
    .bind(firm_id)
    .bind(schedule_id)
    .fetch_optional(&pg.0)
    .await?
    .map(|row| schedule_from_row(&row))
    .transpose()
}

fn schedule_from_row(row: &sqlx::postgres::PgRow) -> Result<PlanFeeSchedule, LegalPlanError> {
    Ok(PlanFeeSchedule {
        id: row.try_get("id")?,
        firm_id: row.try_get("firm_id")?,
        legal_plan_id: row.try_get("legal_plan_id")?,
        matter_type: PlanMatterType::parse(&row.try_get::<String, _>("matter_type")?)?,
        state: row.try_get("state")?,
        covered_benefit: row.try_get("covered_benefit")?,
        plan_fee_cents: row.try_get("plan_fee_cents")?,
        supplemental_hourly_cents: row.try_get("supplemental_hourly_cents")?,
        supplemental_flat_cents: row.try_get("supplemental_flat_cents")?,
        effective_on: row.try_get("effective_on")?,
        inserted_at: row.try_get("inserted_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::{LegalPlanError, NewLegalPlan, NewPlanFeeSchedule, PlanMatterType};

    fn plan() -> NewLegalPlan {
        NewLegalPlan {
            name: "ARAG".into(),
            contact: Some("group@example.com".into()),
            portal: None,
            acceptance_sla_hours: 24,
            turnaround_sla_days: 10,
            reporting_requirements: None,
        }
    }

    fn schedule(plan_id: uuid::Uuid, firm_id: uuid::Uuid) -> NewPlanFeeSchedule {
        NewPlanFeeSchedule {
            firm_id,
            legal_plan_id: plan_id,
            matter_type: PlanMatterType::DocumentReview,
            state: "ny".into(),
            covered_benefit: "Will review".into(),
            plan_fee_cents: 15_000,
            supplemental_hourly_cents: Some(25_000),
            supplemental_flat_cents: None,
            effective_on: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        }
    }

    #[test]
    fn a_plan_needs_positive_service_levels_and_a_name() {
        assert!(plan().validate().is_ok());
        let mut no_name = plan();
        no_name.name = "   ".into();
        assert!(matches!(
            no_name.validate(),
            Err(LegalPlanError::Invalid("name"))
        ));
        for field in ["acceptance_sla_hours", "turnaround_sla_days"] {
            let mut bad = plan();
            if field == "acceptance_sla_hours" {
                bad.acceptance_sla_hours = 0;
            } else {
                bad.turnaround_sla_days = -1;
            }
            let error = bad.validate().unwrap_err();
            match error {
                LegalPlanError::Invalid(found) => {
                    assert_eq!(found, field, "the invalid field is named");
                }
                _ => panic!("the SLA check failed, not something else"),
            }
        }
    }

    #[test]
    fn matter_type_round_trips_its_stored_word() {
        for matter_type in [
            PlanMatterType::DocumentReview,
            PlanMatterType::Letter,
            PlanMatterType::UncontestedDivorce,
            PlanMatterType::CourtAppearance,
        ] {
            assert_eq!(
                PlanMatterType::parse(matter_type.as_str()).unwrap(),
                matter_type
            );
        }
        assert!(PlanMatterType::parse("divorce").is_err());
    }

    #[test]
    fn a_schedule_needs_a_jurisdiction_code_and_nonnegative_cents() {
        assert!(schedule(uuid::Uuid::new_v4(), uuid::Uuid::new_v4())
            .validate()
            .is_ok());
        for (state, field) in [
            ("NY", "state"),
            ("n", "state"),
            ("ny wa", "state"),
            ("naïve", "state"),
        ] {
            let mut bad = schedule(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
            bad.state = state.into();
            let error = bad.validate().unwrap_err();
            match error {
                LegalPlanError::Invalid(found) => {
                    assert_eq!(found, field, "the invalid field is named");
                }
                _ => panic!("the state check failed, not something else"),
            }
        }
        let mut no_fee = schedule(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        no_fee.plan_fee_cents = -1;
        assert!(matches!(
            no_fee.validate(),
            Err(LegalPlanError::Invalid("plan_fee_cents"))
        ));
        let mut hourly = schedule(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        hourly.supplemental_hourly_cents = Some(-1);
        assert!(matches!(
            hourly.validate(),
            Err(LegalPlanError::Invalid("supplemental_hourly_cents"))
        ));
        let mut flat = schedule(uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
        flat.supplemental_flat_cents = Some(-1);
        assert!(matches!(
            flat.validate(),
            Err(LegalPlanError::Invalid("supplemental_flat_cents"))
        ));
    }

    // The remaining assertions need a live PostgreSQL: the Firm-scoped reads,
    mod database {
        use super::super::{
            add_fee_schedule, create, fee_history, find, list, resolve_fee, LegalPlanError,
            NewPlanFeeSchedule, PlanMatterType,
        };
        use super::plan;
        use crate::postgres::PostgresDb;
        use crate::postgres_mirror::tests::{connect, fresh_schema};
        use chrono::NaiveDate;
        use sqlx::Row;
        use uuid::Uuid;

        async fn firm(pg: &PostgresDb) -> Uuid {
            let id: Uuid = sqlx::query(
                "INSERT INTO firm (id,name,slug,status) VALUES ($1,'Synthetic Plan Firm',$2,'active') RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(format!("pf-{}", Uuid::new_v4().simple()))
            .fetch_one(pg.pool())
            .await
            .unwrap()
            .get(0);
            id
        }

        fn day(month: u32, day: u32) -> NaiveDate {
            NaiveDate::from_ymd_opt(2026, month, day).unwrap()
        }

        fn schedule(plan_id: Uuid, firm_id: Uuid) -> NewPlanFeeSchedule {
            NewPlanFeeSchedule {
                firm_id,
                legal_plan_id: plan_id,
                matter_type: PlanMatterType::DocumentReview,
                state: "ny".into(),
                covered_benefit: "Will review".into(),
                plan_fee_cents: 15_000,
                supplemental_hourly_cents: Some(25_000),
                supplemental_flat_cents: None,
                effective_on: day(1, 1),
            }
        }

        #[tokio::test]
        async fn a_plan_is_readable_only_inside_its_firm() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let first = firm(&pg).await;
            let second = firm(&pg).await;
            let created = create(&pg, first, &plan()).await.unwrap();
            assert_eq!(created.firm_id, first);
            assert_eq!(created.acceptance_sla_hours, 24);

            // Another Firm holds no copy and cannot read the row by id.
            assert!(list(&pg, second).await.unwrap().is_empty());
            assert!(find(&pg, second, created.id).await.unwrap().is_none());

            // The same program name is one row per Firm.
            assert!(matches!(
                create(&pg, first, &plan()).await,
                Err(LegalPlanError::Database(_))
            ));
            let same_name_elsewhere = create(&pg, second, &plan()).await.unwrap();
            assert_eq!(same_name_elsewhere.name, created.name);
            assert_ne!(same_name_elsewhere.id, created.id);
        }

        #[tokio::test]
        async fn fee_resolution_takes_the_latest_effective_row() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let firm_id = firm(&pg).await;
            let plan = create(&pg, firm_id, &plan()).await.unwrap();
            assert_eq!(
                resolve_fee(
                    &pg,
                    firm_id,
                    plan.id,
                    PlanMatterType::DocumentReview,
                    "ny",
                    day(12, 31)
                )
                .await
                .unwrap(),
                None,
                "a plan with no schedule resolves to none"
            );
            let january = add_fee_schedule(&pg, &schedule(plan.id, firm_id))
                .await
                .unwrap();
            let mut march = schedule(plan.id, firm_id);
            march.effective_on = day(3, 1);
            march.plan_fee_cents = 17_500;
            march.supplemental_hourly_cents = None;
            let march = add_fee_schedule(&pg, &march).await.unwrap();
            assert_eq!(
                resolve_fee(
                    &pg,
                    firm_id,
                    plan.id,
                    PlanMatterType::DocumentReview,
                    "ny",
                    day(2, 15)
                )
                .await
                .unwrap(),
                Some(january.clone()),
                "a later fee never governs an earlier date"
            );
            assert_eq!(
                resolve_fee(
                    &pg,
                    firm_id,
                    plan.id,
                    PlanMatterType::DocumentReview,
                    "ny",
                    day(12, 31)
                )
                .await
                .unwrap(),
                Some(march.clone())
            );
            assert_eq!(
                resolve_fee(
                    &pg,
                    firm_id,
                    plan.id,
                    PlanMatterType::Letter,
                    "ny",
                    day(12, 31)
                )
                .await
                .unwrap(),
                None,
                "another matter type prices separately"
            );
            assert_eq!(
                resolve_fee(
                    &pg,
                    firm_id,
                    plan.id,
                    PlanMatterType::DocumentReview,
                    "nv",
                    day(12, 31)
                )
                .await
                .unwrap(),
                None,
                "another state prices separately"
            );
            assert_eq!(
                fee_history(&pg, firm_id, plan.id, PlanMatterType::DocumentReview, "ny")
                    .await
                    .unwrap(),
                vec![january, march],
                "both rows remain as history"
            );
        }

        #[tokio::test]
        async fn the_database_refuses_foreign_plans_and_mutation() {
            let Some((options, database)) = fresh_schema().await else {
                return;
            };
            let pg = connect(&options, &database).await;
            let own = firm(&pg).await;
            let other = firm(&pg).await;
            let own_plan = create(&pg, own, &plan()).await.unwrap();
            let foreign_plan = create(&pg, other, &plan()).await.unwrap();

            let mut foreign = schedule(own_plan.id, own);
            foreign.legal_plan_id = foreign_plan.id;
            assert!(matches!(
                add_fee_schedule(&pg, &foreign).await,
                Err(LegalPlanError::PlanNotInFirm)
            ));
            // The accessor is not the only writer the database trusts: a raw
            // insert naming another Firm's plan is refused by the trigger.
            let raw = sqlx::query(
                "INSERT INTO plan_fee_schedule \
                 (firm_id,legal_plan_id,matter_type,state,covered_benefit,plan_fee_cents,effective_on) \
                 VALUES ($1,$2,'document_review','ny','Foreign benefit',1000,$3)",
            )
            .bind(own)
            .bind(foreign_plan.id)
            .bind(day(1, 1))
            .execute(pg.pool())
            .await;
            assert!(raw.is_err(), "the trigger refuses a foreign plan");

            let recorded = add_fee_schedule(&pg, &schedule(own_plan.id, own))
                .await
                .unwrap();
            for statement in [
                "UPDATE plan_fee_schedule SET plan_fee_cents=1 WHERE id=$1",
                "DELETE FROM plan_fee_schedule WHERE id=$1",
            ] {
                assert!(
                    sqlx::query(statement)
                        .bind(recorded.id)
                        .execute(pg.pool())
                        .await
                        .is_err(),
                    "{statement}"
                );
            }
            assert!(
                sqlx::raw_sql("TRUNCATE plan_fee_schedule")
                    .execute(pg.pool())
                    .await
                    .is_err(),
                "truncate is refused like any other mutation"
            );
            assert_eq!(
                fee_history(&pg, own, own_plan.id, PlanMatterType::DocumentReview, "ny")
                    .await
                    .unwrap(),
                vec![recorded],
                "the refused mutations left the history intact"
            );
        }
    }
}
