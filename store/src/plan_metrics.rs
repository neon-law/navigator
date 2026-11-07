//! The Firm's plan-work measurement (ENG-1269): the numbers the CEO digest
//! and the plan pages read, one query each, all Firm-keyed.
//!
//! Every function answers one question over `project`, `time_entry`,
//! `legal_plan`, and `plan_fee_schedule` in plain SQL, filtered to one
//! `firm_id` on every row they touch — a second Firm's matters, plans, or
//! schedule rows never appear. Reads run against PostgreSQL: `time_entry`
//! and the plan tables are PostgreSQL-native, and the plan columns on
//! `project` arrive through the tenancy mirror, which is what the mirror is
//! kept for. The workflow layer (ENG-1264) wraps each function in a
//! journaled step, so a replay recomputes the same number instead of
//! re-deriving it.
//!
//! Money follows the [`crate::finance_aggregate`] rule: integer cents,
//! never floats; a matter's time cost is the sum of its per-entry truncated
//! pieces (`crate::time_entries::entry_amount_cents`), and a group total
//! sums the per-matter pieces rather than re-splitting. Rates over time are
//! rounded half away from zero — the signed form of the same rule.
//!
//! An actor-facing read routes through [`crate::firm_capability`] (a
//! [`PlanMetricsError::Denied`] instead of the rows when the actor may not
//! see the Firm's economics); the durable workflows call the Firm-keyed
//! queries directly, as the system actor. See `docs/glossary/legal-plan.md`.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::legal_plans::PlanMatterType;
use crate::postgres::PostgresDb;

#[derive(Debug, thiserror::Error)]
pub enum PlanMetricsError {
    #[error("invalid plan metrics request: {0}")]
    Invalid(&'static str),
    #[error("reading the Firm's plan metrics is not permitted")]
    Denied,
    #[error("capability resolution failed")]
    Capability(#[from] crate::firms::FirmError),
    #[error("database operation failed")]
    Database(#[from] sqlx::Error),
}

/// Matters counted per Legal Plan and jurisdiction over one window (the
/// digest's month). Each count is its own event in the window: a matter
/// assigned in October and closed in November counts as closed — not
/// assigned — in November's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanMatterCounts {
    pub legal_plan_id: Uuid,
    pub plan_name: String,
    /// The matter's `plan_state`; unset on a plan matter not yet staged
    /// with a jurisdiction.
    pub state: Option<String>,
    /// Matters with `plan_assigned_on` in the window.
    pub assigned: i64,
    /// Matters with `plan_accepted_on` in the window.
    pub accepted: i64,
    /// Matters with `plan_closed_on` in the window.
    pub closed: i64,
}

/// One acceptance-SLA miss: the Firm accepted later than the plan's
/// `acceptance_sla_hours` allows. Elapsed time is the whole days between
/// the plan dates times 24 — DATE-granular, so a same-day acceptance never
/// misses and a next-day acceptance misses only when the budget is under 24
/// hours. The hour-true clock is the deadline row the intake flow (ENG-1270)
/// records from the email landing time; this query measures the captured
/// plan columns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcceptanceSlaMiss {
    pub project_id: Uuid,
    pub code: String,
    pub plan_name: String,
    pub acceptance_sla_hours: i32,
    pub elapsed_hours: i64,
    pub assigned_on: NaiveDate,
    pub accepted_on: NaiveDate,
}

/// One turnaround-SLA miss: the matter closed later than the plan's
/// `turnaround_sla_days` allow, measured acceptance to close.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnaroundSlaMiss {
    pub project_id: Uuid,
    pub code: String,
    pub plan_name: String,
    pub turnaround_sla_days: i32,
    pub elapsed_days: i64,
    pub accepted_on: NaiveDate,
    pub closed_on: NaiveDate,
}

/// Attorney minutes attributed to one Case Stage. The unstaged bucket
/// (`stage_id: None`) holds entries the nightly project update is still
/// nagging about; it reads as a stage with no label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageMinutes {
    pub stage_id: Option<Uuid>,
    pub stage_label: Option<String>,
    pub entry_count: i64,
    pub minutes: i64,
}

/// One plan matter's economics: what the plan pays (the fee schedule row
/// effective on the matter's acceptance — or assignment, before it is
/// accepted), what the attorney time cost at the snapshotted rates, and the
/// margin between them. A matter with no resolving schedule row is
/// unpriced: `plan_fee_cents` and the margins are `None` rather than a
/// fake zero.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterEconomics {
    pub project_id: Uuid,
    pub code: String,
    pub legal_plan_id: Uuid,
    pub plan_name: String,
    pub matter_type: PlanMatterType,
    pub state: Option<String>,
    /// The resolved schedule row's amounts; the supplemental columns quote
    /// what the plan allows beyond the covered benefit.
    pub plan_fee_cents: Option<i32>,
    pub supplemental_flat_cents: Option<i32>,
    pub supplemental_hourly_cents: Option<i32>,
    /// Total attorney minutes captured on the matter.
    pub minutes: i64,
    /// Sum of the per-entry truncated amounts — the rounded pieces.
    pub time_cost_cents: i64,
    /// Plan fee minus time cost; `None` while the matter is unpriced.
    pub margin_cents: Option<i64>,
    /// Margin over logged time; `None` while unpriced or unmeasured.
    pub margin_cents_per_hour: Option<i64>,
}

/// Matter economics rolled up per matter type (the digest's "per matter
/// type" line). Totals sum the per-matter pieces; the per-hour rate is
/// total margin over total minutes, never an average of averages. Matters
/// whose schedule did not resolve contribute their time cost with no fee —
/// `unpriced_matters` counts them, so a missing schedule row shows itself
/// instead of silently masquerading as a loss.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterTypeEconomics {
    pub matter_type: PlanMatterType,
    pub matter_count: i64,
    pub plan_fee_cents: i64,
    pub time_cost_cents: i64,
    pub minutes: i64,
    pub margin_cents: i64,
    pub margin_cents_per_hour: i64,
    pub unpriced_matters: i64,
}

/// One stage of one template-effect comparison (the digest's template line):
/// plan matters sharing a `plan_matter_type`, split into matters whose
/// first notation of the template code falls on or after the code's
/// first-existence date (the "with" cohort) and the same-type matters
/// without one (the "before" cohort). A stage's average divides its total
/// minutes by the cohort matters that logged any time in that stage, so an
/// unmeasured matter never masquerades as a fast one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateEffect {
    pub template_code: String,
    pub matter_type: PlanMatterType,
    pub stage_id: Option<Uuid>,
    pub stage_label: Option<String>,
    pub with_matters: i64,
    pub with_minutes: i64,
    pub before_matters: i64,
    pub before_minutes: i64,
}

impl TemplateEffect {
    /// Average minutes per "with" matter in this stage, rounded half away
    /// from zero.
    #[must_use]
    pub fn with_average_minutes(&self) -> i64 {
        average_minutes(self.with_minutes, self.with_matters)
    }

    /// Average minutes per "before" matter in this stage, rounded half away
    /// from zero.
    #[must_use]
    pub fn before_average_minutes(&self) -> i64 {
        average_minutes(self.before_minutes, self.before_matters)
    }
}

/// The sections an actor-facing surface renders in one pass — every
/// window-scoped query resolved once, under one capability check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanMetricsReport {
    pub counts: Vec<PlanMatterCounts>,
    pub acceptance_misses: Vec<AcceptanceSlaMiss>,
    pub turnaround_misses: Vec<TurnaroundSlaMiss>,
    pub economics: Vec<MatterEconomics>,
    pub matter_type_economics: Vec<MatterTypeEconomics>,
}

/// Round half away from zero: the signed form of the `finance_aggregate`
/// rounding rule ("a share is `amount * percent / 100`, rounded half up").
/// Halfway values leave the zero side, so a negative rate rounds down —
/// a loss is never flattered toward zero.
#[must_use]
pub fn round_half_away_from_zero(numerator: i64, denominator: i64) -> i64 {
    debug_assert!(denominator > 0, "denominator must be positive");
    if numerator >= 0 {
        (2 * numerator + denominator) / (2 * denominator)
    } else {
        -((-2 * numerator + denominator) / (2 * denominator))
    }
}

/// Margin over logged time, in whole cents per hour. Zero minutes measures
/// nothing, so the rate is 0 rather than a division.
#[must_use]
pub fn margin_cents_per_hour(margin_cents: i64, minutes: i64) -> i64 {
    if minutes <= 0 {
        return 0;
    }
    round_half_away_from_zero(margin_cents * 60, minutes)
}

/// Average minutes per matter, rounded half away from zero; no matters
/// measures nothing.
#[must_use]
pub fn average_minutes(total_minutes: i64, matters: i64) -> i64 {
    if matters <= 0 {
        return 0;
    }
    round_half_away_from_zero(total_minutes, matters)
}

/// Matters per Legal Plan and jurisdiction, each count its own event in
/// the window. Plan matters only: matters with no `legal_plan_id` are the
/// other practice lines, which the firm digest separates on its own.
///
/// # Errors
///
/// [`PlanMetricsError::Database`] on a database failure.
pub async fn matter_counts(
    pg: &PostgresDb,
    firm_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<PlanMatterCounts>, PlanMetricsError> {
    let rows = sqlx::query(
        "SELECT lp.id AS legal_plan_id, lp.name AS plan_name, p.plan_state AS state, \
         COUNT(*) FILTER (WHERE p.plan_assigned_on BETWEEN $2 AND $3) AS assigned, \
         COUNT(*) FILTER (WHERE p.plan_accepted_on BETWEEN $2 AND $3) AS accepted, \
         COUNT(*) FILTER (WHERE p.plan_closed_on BETWEEN $2 AND $3) AS closed \
         FROM project p \
         JOIN legal_plan lp ON lp.id = p.legal_plan_id AND lp.firm_id = $1 \
         WHERE p.firm_id = $1 AND p.legal_plan_id IS NOT NULL \
         GROUP BY lp.id, lp.name, p.plan_state \
         HAVING COUNT(*) FILTER (WHERE p.plan_assigned_on BETWEEN $2 AND $3) > 0 \
            OR COUNT(*) FILTER (WHERE p.plan_accepted_on BETWEEN $2 AND $3) > 0 \
            OR COUNT(*) FILTER (WHERE p.plan_closed_on BETWEEN $2 AND $3) > 0 \
         ORDER BY lp.name, p.plan_state NULLS LAST",
    )
    .bind(firm_id)
    .bind(from)
    .bind(to)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(PlanMatterCounts {
            legal_plan_id: row.try_get("legal_plan_id")?,
            plan_name: row.try_get("plan_name")?,
            state: row.try_get("state")?,
            assigned: row.try_get("assigned")?,
            accepted: row.try_get("accepted")?,
            closed: row.try_get("closed")?,
        })
    })
    .collect::<Result<Vec<_>, PlanMetricsError>>()?;
    Ok(rows)
}

/// Acceptance-SLA misses among matters accepted in the window. See
/// [`AcceptanceSlaMiss`] for the DATE-granular elapsed-time rule.
///
/// # Errors
///
/// [`PlanMetricsError::Database`] on a database failure.
pub async fn acceptance_sla_misses(
    pg: &PostgresDb,
    firm_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<AcceptanceSlaMiss>, PlanMetricsError> {
    let rows = sqlx::query(
        "SELECT p.id AS project_id, p.code, lp.name AS plan_name, \
         lp.acceptance_sla_hours, \
         ((p.plan_accepted_on - p.plan_assigned_on) * 24)::bigint AS elapsed_hours, \
         p.plan_assigned_on, p.plan_accepted_on \
         FROM project p \
         JOIN legal_plan lp ON lp.id = p.legal_plan_id \
         WHERE p.firm_id = $1 AND lp.firm_id = $1 \
           AND p.plan_assigned_on IS NOT NULL AND p.plan_accepted_on IS NOT NULL \
           AND p.plan_accepted_on BETWEEN $2 AND $3 \
           AND (p.plan_accepted_on - p.plan_assigned_on) * 24 > lp.acceptance_sla_hours \
         ORDER BY p.plan_accepted_on, p.id",
    )
    .bind(firm_id)
    .bind(from)
    .bind(to)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(AcceptanceSlaMiss {
            project_id: row.try_get("project_id")?,
            code: row.try_get("code")?,
            plan_name: row.try_get("plan_name")?,
            acceptance_sla_hours: row.try_get("acceptance_sla_hours")?,
            elapsed_hours: row.try_get("elapsed_hours")?,
            assigned_on: row.try_get("assigned_on")?,
            accepted_on: row.try_get("accepted_on")?,
        })
    })
    .collect::<Result<Vec<_>, PlanMetricsError>>()?;
    Ok(rows)
}

/// Turnaround-SLA misses among matters closed in the window, measured
/// acceptance to close.
///
/// # Errors
///
/// [`PlanMetricsError::Database`] on a database failure.
pub async fn turnaround_sla_misses(
    pg: &PostgresDb,
    firm_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<TurnaroundSlaMiss>, PlanMetricsError> {
    let rows = sqlx::query(
        "SELECT p.id AS project_id, p.code, lp.name AS plan_name, \
         lp.turnaround_sla_days, \
         (p.plan_closed_on - p.plan_accepted_on)::bigint AS elapsed_days, \
         p.plan_accepted_on, p.plan_closed_on \
         FROM project p \
         JOIN legal_plan lp ON lp.id = p.legal_plan_id \
         WHERE p.firm_id = $1 AND lp.firm_id = $1 \
           AND p.plan_accepted_on IS NOT NULL AND p.plan_closed_on IS NOT NULL \
           AND p.plan_closed_on BETWEEN $2 AND $3 \
           AND (p.plan_closed_on - p.plan_accepted_on) > lp.turnaround_sla_days \
         ORDER BY p.plan_closed_on, p.id",
    )
    .bind(firm_id)
    .bind(from)
    .bind(to)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(TurnaroundSlaMiss {
            project_id: row.try_get("project_id")?,
            code: row.try_get("code")?,
            plan_name: row.try_get("plan_name")?,
            turnaround_sla_days: row.try_get("turnaround_sla_days")?,
            elapsed_days: row.try_get("elapsed_days")?,
            accepted_on: row.try_get("accepted_on")?,
            closed_on: row.try_get("closed_on")?,
        })
    })
    .collect::<Result<Vec<_>, PlanMetricsError>>()?;
    Ok(rows)
}

/// Attorney minutes by Case Stage across every plan matter on one Legal
/// Plan, including the unstaged bucket. Ordered by the Firm's stage
/// positions.
///
/// # Errors
///
/// [`PlanMetricsError::Database`] on a database failure.
pub async fn minutes_by_stage_for_plan(
    pg: &PostgresDb,
    firm_id: Uuid,
    legal_plan_id: Uuid,
) -> Result<Vec<StageMinutes>, PlanMetricsError> {
    stage_minutes(
        pg,
        "JOIN legal_plan lp ON lp.id = p.legal_plan_id \
         WHERE p.firm_id = $1 AND lp.firm_id = $1 AND lp.id = $2 \
           AND p.legal_plan_id IS NOT NULL",
        firm_id,
        legal_plan_id,
    )
    .await
}

/// Attorney minutes by Case Stage on one matter, including the unstaged
/// bucket. A matter of another Firm answers nothing — the Firm filter
/// holds on every read.
///
/// # Errors
///
/// [`PlanMetricsError::Database`] on a database failure.
pub async fn minutes_by_stage_for_project(
    pg: &PostgresDb,
    firm_id: Uuid,
    project_id: Uuid,
) -> Result<Vec<StageMinutes>, PlanMetricsError> {
    stage_minutes(
        pg,
        "WHERE p.firm_id = $1 AND p.id = $2",
        firm_id,
        project_id,
    )
    .await
}

/// The shared stage-minutes aggregation: one Firm filter, one matter or
/// plan selector, one `GROUP BY` over the stage vocabulary.
async fn stage_minutes(
    pg: &PostgresDb,
    selector: &str,
    firm_id: Uuid,
    subject: Uuid,
) -> Result<Vec<StageMinutes>, PlanMetricsError> {
    // The selector is a compile-time constant — no caller input reaches
    // the SQL text; the ids arrive as binds.
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT te.stage AS stage_id, sd.label AS stage_label, \
         COUNT(*)::bigint AS entry_count, SUM(te.minutes)::bigint AS minutes \
         FROM time_entry te \
         JOIN project p ON p.id = te.project_id \
         LEFT JOIN stage_definition sd ON sd.id = te.stage \
         {selector} \
         GROUP BY te.stage, sd.label, sd.position \
         ORDER BY sd.position NULLS LAST, te.stage NULLS LAST"
    )))
    .bind(firm_id)
    .bind(subject)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(StageMinutes {
            stage_id: row.try_get("stage_id")?,
            stage_label: row.try_get("stage_label")?,
            entry_count: row.try_get("entry_count")?,
            minutes: row.try_get("minutes")?,
        })
    })
    .collect::<Result<Vec<_>, PlanMetricsError>>()?;
    Ok(rows)
}

/// Per-matter plan economics for matters assigned in the window: the
/// resolved schedule fee, the summed entry-piece time cost, and the margin.
///
/// # Errors
///
/// [`PlanMetricsError::Database`] on a database failure.
pub async fn matter_economics(
    pg: &PostgresDb,
    firm_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<MatterEconomics>, PlanMetricsError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(economics_sql(
        "SELECT m.id AS project_id, m.code, m.legal_plan_id, m.plan_name, \
         m.plan_matter_type, m.plan_state, f.plan_fee_cents, \
         f.supplemental_flat_cents, f.supplemental_hourly_cents, \
         COALESCE(tc.minutes, 0)::bigint AS minutes, \
         COALESCE(tc.time_cost_cents, 0)::bigint AS time_cost_cents \
         FROM matters m \
         LEFT JOIN fee f ON f.id = m.id \
         LEFT JOIN time_cost tc ON tc.project_id = m.id \
         ORDER BY m.code",
    )))
    .bind(firm_id)
    .bind(from)
    .bind(to)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        let plan_fee_cents: Option<i32> = row.try_get("plan_fee_cents")?;
        let minutes: i64 = row.try_get("minutes")?;
        let time_cost_cents: i64 = row.try_get("time_cost_cents")?;
        let margin_cents = plan_fee_cents.map(i64::from).map(|fee| fee - time_cost_cents);
        Ok(MatterEconomics {
            project_id: row.try_get("project_id")?,
            code: row.try_get("code")?,
            legal_plan_id: row.try_get("legal_plan_id")?,
            plan_name: row.try_get("plan_name")?,
            matter_type: parse_matter_type(&row.try_get::<String, _>("plan_matter_type")?)?,
            state: row.try_get("plan_state")?,
            plan_fee_cents,
            supplemental_flat_cents: row.try_get("supplemental_flat_cents")?,
            supplemental_hourly_cents: row.try_get("supplemental_hourly_cents")?,
            minutes,
            time_cost_cents,
            margin_cents,
            margin_cents_per_hour: margin_cents.map(|margin| margin_cents_per_hour(margin, minutes)),
        })
    })
    .collect::<Result<Vec<_>, PlanMetricsError>>()?;
    Ok(rows)
}

/// Per-type rollup of [`matter_economics`]: totals sum the per-matter
/// pieces, and the per-hour rate divides total margin by total minutes —
/// never an average of the per-matter rates.
///
/// # Errors
///
/// [`PlanMetricsError::Database`] on a database failure.
pub async fn margin_by_matter_type(
    pg: &PostgresDb,
    firm_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<MatterTypeEconomics>, PlanMetricsError> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(economics_sql(
        "SELECT plan_matter_type, \
         COUNT(*)::bigint AS matter_count, \
         COALESCE(SUM(CASE WHEN unpriced THEN 0 ELSE plan_fee_cents END), 0)::bigint AS plan_fee_cents, \
         COALESCE(SUM(time_cost_cents), 0)::bigint AS time_cost_cents, \
         COALESCE(SUM(minutes), 0)::bigint AS minutes, \
         COUNT(*) FILTER (WHERE unpriced)::bigint AS unpriced_matters \
         FROM per_matter \
         GROUP BY plan_matter_type \
         ORDER BY plan_matter_type",
    )))
    .bind(firm_id)
    .bind(from)
    .bind(to)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        let matter_type = parse_matter_type(&row.try_get::<String, _>("plan_matter_type")?)?;
        let plan_fee_cents: i64 = row.try_get("plan_fee_cents")?;
        let time_cost_cents: i64 = row.try_get("time_cost_cents")?;
        let minutes: i64 = row.try_get("minutes")?;
        let margin_cents = plan_fee_cents - time_cost_cents;
        Ok(MatterTypeEconomics {
            matter_type,
            matter_count: row.try_get("matter_count")?,
            plan_fee_cents,
            time_cost_cents,
            minutes,
            margin_cents,
            margin_cents_per_hour: margin_cents_per_hour(margin_cents, minutes),
            unpriced_matters: row.try_get("unpriced_matters")?,
        })
    })
    .collect::<Result<Vec<_>, PlanMetricsError>>()?;
    Ok(rows)
}

/// The CTEs both economics queries share: the window's plan matters, their
/// summed entry-piece time costs, and the fee schedule row effective when
/// each matter was priced — the attorney_rate rule, one row per matter.
fn economics_sql(final_select: &str) -> String {
    format!(
        "WITH matters AS ( \
             SELECT p.id, p.code, p.legal_plan_id, lp.name AS plan_name, \
                    p.plan_matter_type, p.plan_state, \
                    COALESCE(p.plan_accepted_on, p.plan_assigned_on) AS priced_on \
             FROM project p \
             JOIN legal_plan lp ON lp.id = p.legal_plan_id AND lp.firm_id = p.firm_id \
             WHERE p.firm_id = $1 AND p.plan_matter_type IS NOT NULL \
               AND p.plan_assigned_on BETWEEN $2 AND $3 \
         ), \
         time_cost AS ( \
             SELECT te.project_id, \
                    SUM(te.minutes)::bigint AS minutes, \
                    SUM((te.minutes::bigint * te.rate_cents_per_hour) / 60)::bigint AS time_cost_cents \
             FROM time_entry te \
             JOIN matters m ON m.id = te.project_id \
             GROUP BY te.project_id \
         ), \
         fee AS ( \
             SELECT DISTINCT ON (m.id) \
                    m.id, f.plan_fee_cents, f.supplemental_flat_cents, f.supplemental_hourly_cents \
             FROM matters m \
             JOIN plan_fee_schedule f ON f.legal_plan_id = m.legal_plan_id \
                  AND f.firm_id = $1 AND f.matter_type = m.plan_matter_type \
                  AND f.state = m.plan_state AND f.effective_on <= m.priced_on \
             ORDER BY m.id, f.effective_on DESC, f.inserted_at DESC \
         ), \
         per_matter AS ( \
             SELECT m.id, m.plan_matter_type, f.plan_fee_cents, \
                    (f.plan_fee_cents IS NULL) AS unpriced, \
                    COALESCE(tc.minutes, 0)::bigint AS minutes, \
                    COALESCE(tc.time_cost_cents, 0)::bigint AS time_cost_cents \
             FROM matters m \
             LEFT JOIN fee f ON f.id = m.id \
             LEFT JOIN time_cost tc ON tc.project_id = m.id \
         ) \
         {final_select}"
    )
}

/// The template-effect comparison for the named template codes: per code,
/// per matter type, per stage, the "with" cohort (matters whose first
/// notation of the code falls on or after the code's first-existence date —
/// the oldest version row's `inserted_at` in the Firm's scope) against the
/// "before" cohort (same-type plan matters without one). Codes with no
/// template rows in scope answer nothing.
///
/// # Errors
///
/// [`PlanMetricsError::Database`] on a database failure.
pub async fn template_effect(
    pg: &PostgresDb,
    firm_id: Uuid,
    template_codes: &[String],
) -> Result<Vec<TemplateEffect>, PlanMetricsError> {
    let rows = sqlx::query(
        "WITH codes AS (SELECT unnest($1::text[]) AS code), \
         scope AS ( \
             SELECT t.id, t.code, t.inserted_at \
             FROM template t \
             LEFT JOIN project tp ON tp.id = t.project_id \
             JOIN codes c ON c.code = t.code \
             WHERE t.firm_id = $2 OR t.firm_id IS NULL OR tp.firm_id = $2 \
         ), \
         first_existence AS ( \
             SELECT code, MIN(inserted_at) AS first_at FROM scope GROUP BY code \
         ), \
         matters AS ( \
             SELECT p.id, p.plan_matter_type \
             FROM project p \
             WHERE p.firm_id = $2 AND p.legal_plan_id IS NOT NULL \
               AND p.plan_matter_type IS NOT NULL \
         ), \
         first_use AS ( \
             SELECT fe.code, n.project_id AS matter_id, MIN(n.inserted_at) AS first_use_at \
             FROM notation n \
             JOIN template t ON t.id = n.template_id \
             JOIN first_existence fe ON fe.code = t.code \
             JOIN matters m ON m.id = n.project_id \
             GROUP BY fe.code, n.project_id \
         ), \
         cohort AS ( \
             SELECT fe.code, m.id AS matter_id, m.plan_matter_type, \
                    (fu.first_use_at IS NOT NULL AND fu.first_use_at >= fe.first_at) AS used \
             FROM matters m \
             CROSS JOIN first_existence fe \
             LEFT JOIN first_use fu ON fu.code = fe.code AND fu.matter_id = m.id \
         ), \
         stage_minutes AS ( \
             SELECT te.project_id, te.stage AS stage_id, sd.label AS stage_label, \
                    SUM(te.minutes)::bigint AS minutes \
             FROM time_entry te \
             LEFT JOIN stage_definition sd ON sd.id = te.stage \
             WHERE te.project_id IN (SELECT id FROM matters) \
             GROUP BY te.project_id, te.stage, sd.label \
         ) \
         SELECT c.code, c.plan_matter_type, sm.stage_id, sm.stage_label, \
                COUNT(*) FILTER (WHERE c.used)::bigint AS with_matters, \
                COALESCE(SUM(sm.minutes) FILTER (WHERE c.used), 0)::bigint AS with_minutes, \
                COUNT(*) FILTER (WHERE NOT c.used)::bigint AS before_matters, \
                COALESCE(SUM(sm.minutes) FILTER (WHERE NOT c.used), 0)::bigint AS before_minutes \
         FROM cohort c \
         JOIN stage_minutes sm ON sm.project_id = c.matter_id \
         GROUP BY c.code, c.plan_matter_type, sm.stage_id, sm.stage_label \
         ORDER BY c.code, c.plan_matter_type, sm.stage_label NULLS LAST",
    )
    .bind(template_codes.to_vec())
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(TemplateEffect {
            template_code: row.try_get("code")?,
            matter_type: parse_matter_type(&row.try_get::<String, _>("plan_matter_type")?)?,
            stage_id: row.try_get("stage_id")?,
            stage_label: row.try_get("stage_label")?,
            with_matters: row.try_get("with_matters")?,
            with_minutes: row.try_get("with_minutes")?,
            before_matters: row.try_get("before_matters")?,
            before_minutes: row.try_get("before_minutes")?,
        })
    })
    .collect::<Result<Vec<_>, PlanMetricsError>>()?;
    Ok(rows)
}

/// The actor-facing read: every window-scoped section under one
/// [`crate::firm_capability::FirmCapability::ViewPlanMetrics`] check, the
/// way `communication_dashboard::for_brand` routes its report. A Lawyer or
/// Clerk is refused their own Firm's economics; the durable workflows call
/// the Firm-keyed queries directly, as the system actor.
///
/// # Errors
///
/// [`PlanMetricsError::Denied`] when the actor may not read the Firm's
/// plan metrics, [`PlanMetricsError::Capability`] when the resolver itself
/// fails, and [`PlanMetricsError::Database`] on a database failure.
pub async fn firm_report(
    pg: &PostgresDb,
    surreal: &crate::surreal::SurrealDb,
    actor_person_id: Uuid,
    firm_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<PlanMetricsReport, PlanMetricsError> {
    let person = crate::persons::find_by_id(surreal, actor_person_id)
        .await
        .map_err(crate::firms::FirmError::from)?
        .ok_or(PlanMetricsError::Denied)?;
    let decision = crate::firm_capability::FirmCapability::ViewPlanMetrics
        .resolve(surreal, person.role, Some(actor_person_id), firm_id)
        .await?;
    if !decision.is_allowed() {
        return Err(PlanMetricsError::Denied);
    }
    Ok(PlanMetricsReport {
        counts: matter_counts(pg, firm_id, from, to).await?,
        acceptance_misses: acceptance_sla_misses(pg, firm_id, from, to).await?,
        turnaround_misses: turnaround_sla_misses(pg, firm_id, from, to).await?,
        economics: matter_economics(pg, firm_id, from, to).await?,
        matter_type_economics: margin_by_matter_type(pg, firm_id, from, to).await?,
    })
}

/// Parses the stored `plan_matter_type` word. The column's CHECK constraint
/// makes a failure unreachable through a live writer; the typed error keeps
/// a corrupted row from leaking an untyped value into a report.
fn parse_matter_type(raw: &str) -> Result<PlanMatterType, PlanMetricsError> {
    PlanMatterType::parse(raw).map_err(|_| PlanMetricsError::Invalid("plan_matter_type"))
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::time_entries::entry_amount_cents;

    #[test]
    fn rounds_half_away_from_zero() {
        assert_eq!(round_half_away_from_zero(150, 60), 3);
        assert_eq!(round_half_away_from_zero(149, 60), 2);
        assert_eq!(round_half_away_from_zero(30, 60), 1);
        assert_eq!(round_half_away_from_zero(29, 60), 0);
        // The signed form: halfway leaves the zero side, so a loss is
        // never flattered toward zero.
        assert_eq!(round_half_away_from_zero(-30, 60), -1);
        assert_eq!(round_half_away_from_zero(-29, 60), 0);
        assert_eq!(round_half_away_from_zero(-150, 60), -3);
    }

    #[test]
    fn margin_over_logged_time_is_whole_cents_per_hour() {
        assert_eq!(margin_cents_per_hour(0, 120), 0);
        assert_eq!(margin_cents_per_hour(-32_500, 120), -16_250);
        // 10_001 cents over two hours is 5_000.5 cents per hour — the
        // half cent rounds away from zero in both directions.
        assert_eq!(margin_cents_per_hour(10_001, 120), 5_001);
        assert_eq!(margin_cents_per_hour(-10_001, 120), -5_001);
        // No logged time measures nothing; there is no division by zero.
        assert_eq!(margin_cents_per_hour(5_000, 0), 0);
    }

    #[test]
    fn averages_divide_by_matters() {
        assert_eq!(average_minutes(100, 2), 50);
        assert_eq!(average_minutes(101, 2), 51);
        assert_eq!(average_minutes(0, 0), 0);
    }

    #[test]
    fn the_time_cost_is_the_sum_of_the_entry_pieces() {
        // The `finance_aggregate` rule: group totals sum the rounded
        // pieces, never re-split the raw totals — a 1-minute entry at
        // $250/hr truncates to 416 cents, which re-splitting forgets.
        let pieces: i64 = [90, 1, 29]
            .iter()
            .map(|minutes| i64::from(entry_amount_cents(*minutes, 25_000)))
            .sum();
        assert_eq!(pieces, 37_500 + 416 + 12_083);
        let resplit = i64::from(120) * i64::from(25_000) / 60;
        assert_ne!(pieces, resplit, "truncation makes the pieces the rule");
    }
}

#[cfg(test)]
mod database {
    use super::*;
    use crate::firms::{self, FirmMembership, NewFirm, NewPersonFirmRole};
    use crate::legal_plans::{self, NewLegalPlan, NewPlanFeeSchedule};
    use crate::postgres_mirror::tests::{connect, fresh_schema};
    use crate::persons::{self, NewPerson, Role};
    use crate::test_support::{mem_surreal, seed_entity};
    use crate::time_entries::{self, NewTimeEntry, TimeEntrySource};
    use chrono::{DateTime, NaiveDate, TimeZone, Utc};

    fn day(month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, month, day).unwrap_or_else(|| panic!("valid date"))
    }

    fn stamp(hour: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 1, hour, 0, 0).single().unwrap_or_else(|| panic!("valid stamp"))
    }

    /// A Firm, its entity, and one attorney — the rows every metric test
    /// needs before it can matter anything.
    struct FirmFixture {
        firm_id: Uuid,
        entity_id: Uuid,
        attorney: Uuid,
    }

    async fn firm(pg: &PostgresDb) -> Uuid {
        let id = Uuid::new_v4();
        let slug = format!("pm-{}", Uuid::new_v4().simple());
        sqlx::query("INSERT INTO firm (id, name, slug, status) VALUES ($1, 'Synthetic Metrics Firm', $2, 'active')")
            .bind(id)
            .bind(slug)
            .execute(&pg.0)
            .await
            .unwrap_or_else(|error| panic!("firm row: {error}"));
        id
    }

    async fn person(pg: &PostgresDb, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO person (id, name, email, role) VALUES ($1, $2, $3, 'lawyer')")
            .bind(id)
            .bind(name)
            .bind(format!("{name}@metrics.example.com"))
            .execute(&pg.0)
            .await
            .unwrap_or_else(|error| panic!("person row: {error}"));
        id
    }

    async fn entity(pg: &PostgresDb) -> Uuid {
        let jurisdiction: Uuid = sqlx::query(
            "INSERT INTO jurisdiction (id, name, code) VALUES ($1, 'Synthetic Metrics Jurisdiction', 'PMJ') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .fetch_one(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("jurisdiction row: {error}"))
        .try_get(0)
        .unwrap_or_else(|error| panic!("jurisdiction id: {error}"));
        let entity_type: Uuid = sqlx::query(
            "INSERT INTO entity_type (id, name) VALUES ($1, 'Synthetic Metrics Entity Type') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .fetch_one(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("entity_type row: {error}"))
        .try_get(0)
        .unwrap_or_else(|error| panic!("entity_type id: {error}"));
        let id: Uuid = sqlx::query(
            "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id) \
             VALUES ($1, 'Synthetic Metrics Entity', $2, $3) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(entity_type)
        .bind(jurisdiction)
        .fetch_one(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("entity row: {error}"))
        .try_get(0)
        .unwrap_or_else(|error| panic!("entity id: {error}"));
        id
    }

    async fn seed_firm(pg: &PostgresDb) -> FirmFixture {
        FirmFixture {
            firm_id: firm(pg).await,
            entity_id: entity(pg).await,
            attorney: person(pg, "Attorney").await,
        }
    }

    async fn plan(pg: &PostgresDb, firm_id: Uuid, name: &str) -> Uuid {
        legal_plans::create(
            pg,
            firm_id,
            &NewLegalPlan {
                name: name.to_owned(),
                contact: None,
                portal: None,
                acceptance_sla_hours: 24,
                turnaround_sla_days: 30,
                reporting_requirements: None,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("legal plan: {error}"))
        .id
    }

    async fn schedule(
        pg: &PostgresDb,
        firm_id: Uuid,
        legal_plan_id: Uuid,
        matter_type: &PlanMatterType,
        plan_fee_cents: i32,
        effective_on: NaiveDate,
    ) -> Uuid {
        legal_plans::add_fee_schedule(
            pg,
            &NewPlanFeeSchedule {
                firm_id,
                legal_plan_id,
                matter_type: *matter_type,
                state: "ny".to_owned(),
                covered_benefit: "Synthetic covered benefit".to_owned(),
                plan_fee_cents,
                supplemental_hourly_cents: None,
                supplemental_flat_cents: None,
                effective_on,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("fee schedule: {error}"))
        .id
    }

    #[allow(clippy::too_many_arguments)]
    async fn matter(
        pg: &PostgresDb,
        fixture: &FirmFixture,
        code: &str,
        plan_id: Option<Uuid>,
        matter_type: Option<&str>,
        state: Option<&str>,
        assigned_on: Option<NaiveDate>,
        accepted_on: Option<NaiveDate>,
        closed_on: Option<NaiveDate>,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO project (id, code, name, status, entity_id, firm_id, legal_plan_id, \
             plan_matter_type, plan_state, plan_assigned_on, plan_accepted_on, plan_closed_on) \
             VALUES ($1, $2, 'Synthetic Metrics Matter', 'active', $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(id)
        .bind(code)
        .bind(fixture.entity_id)
        .bind(fixture.firm_id)
        .bind(plan_id)
        .bind(matter_type)
        .bind(state)
        .bind(assigned_on)
        .bind(accepted_on)
        .bind(closed_on)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("matter row: {error}"));
        id
    }

    async fn stage(pg: &PostgresDb, firm_id: Uuid, label: &str, position: i32) -> Uuid {
        sqlx::query(
            "INSERT INTO stage_definition (id, firm_id, label, position, inserted_at, updated_at) \
             VALUES ($1, $2, $3, $4, now(), now()) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(firm_id)
        .bind(label)
        .bind(position)
        .fetch_one(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("stage row: {error}"))
        .try_get(0)
        .unwrap_or_else(|error| panic!("stage id: {error}"))
    }

    /// A time entry on the matter, attributed to the attorney. The
    /// participation row the entry requires is added idempotently.
    async fn entry(
        pg: &PostgresDb,
        project_id: Uuid,
        attorney: Uuid,
        minutes: i32,
        rate_cents_per_hour: i32,
        stage_id: Option<Uuid>,
    ) -> Uuid {
        sqlx::query(
            "INSERT INTO person_project_role (person_id, project_id, participation) \
             VALUES ($1, $2, 'lawyer') ON CONFLICT DO NOTHING",
        )
        .bind(attorney)
        .bind(project_id)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("participation row: {error}"));
        let created = time_entries::create(
            pg,
            project_id,
            attorney,
            &NewTimeEntry {
                attorney_id: attorney,
                performed_on: day(11, 5),
                minutes,
                description: "Synthetic metrics work".to_owned(),
                billable: true,
                rate_cents_per_hour,
                source: TimeEntrySource::Manual,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("time entry: {error}"));
        if let Some(stage_id) = stage_id {
            time_entries::set_stage(pg, created.id, attorney, Some(stage_id))
                .await
                .unwrap_or_else(|error| panic!("stage assignment: {error}"));
        }
        created.id
    }

    async fn template_version(
        pg: &PostgresDb,
        code: &str,
        firm_id: Option<Uuid>,
        inserted_at: DateTime<Utc>,
        is_current: bool,
    ) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO template (id, code, title, respondent_type, project_id, firm_id, \
             is_current, inserted_at, updated_at) \
             VALUES ($1, $2, $3, 'matter', NULL, $4, $5, $6, $6)",
        )
        .bind(id)
        .bind(code)
        .bind(format!("Synthetic {code}"))
        .bind(firm_id)
        .bind(is_current)
        .bind(inserted_at)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("template row: {error}"));
        id
    }

    async fn notation(
        pg: &PostgresDb,
        template_id: Uuid,
        person_id: Uuid,
        project_id: Uuid,
        inserted_at: DateTime<Utc>,
    ) {
        sqlx::query(
            "INSERT INTO notation (id, template_id, person_id, project_id, state, \
             inserted_at, updated_at) VALUES ($1, $2, $3, $4, 'prepared', $5, $5)",
        )
        .bind(Uuid::new_v4())
        .bind(template_id)
        .bind(person_id)
        .bind(project_id)
        .bind(inserted_at)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("notation row: {error}"));
    }

    #[tokio::test]
    async fn matter_counts_are_firm_keyed_and_count_events_in_the_window() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let a = seed_firm(&pg).await;
        let b = seed_firm(&pg).await;
        let arag_a = plan(&pg, a.firm_id, "ARAG").await;
        let shield_a = plan(&pg, a.firm_id, "LegalShield").await;

        // Firm A: two ARAG New York matters (one still open), one ARAG
        // California matter, one LegalShield matter never staged with a
        // state, and one assigned before the window.
        let _ = matter(&pg, &a, "plan-a1", Some(arag_a), Some("document_review"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 3)), Some(day(11, 6))).await;
        let _ = matter(&pg, &a, "plan-a2", Some(arag_a), Some("document_review"), Some("ny"),
            Some(day(11, 10)), None, None).await;
        let _ = matter(&pg, &a, "plan-a3", Some(arag_a), Some("letter"), Some("ca"),
            Some(day(11, 12)), Some(day(11, 13)), None).await;
        let _ = matter(&pg, &a, "plan-a4", Some(shield_a), None, None,
            Some(day(11, 14)), None, None).await;
        let _ = matter(&pg, &a, "plan-a5", Some(arag_a), Some("document_review"), Some("ny"),
            Some(day(10, 30)), Some(day(10, 30)), Some(day(10, 31))).await;

        let counts = matter_counts(&pg, a.firm_id, day(11, 1), day(11, 30))
            .await
            .unwrap_or_else(|error| panic!("counts: {error}"));
        assert_eq!(counts.len(), 3, "two ARAG state buckets plus LegalShield");
        let new_york = counts
            .iter()
            .find(|row| row.plan_name == "ARAG" && row.state.as_deref() == Some("ny"))
            .unwrap_or_else(|| panic!("ARAG New York bucket"));
        assert_eq!(
            (new_york.assigned, new_york.accepted, new_york.closed),
            (2, 1, 1),
            "the window counts its own events: a10's November dates never appear"
        );
        let legal_shield = counts
            .iter()
            .find(|row| row.plan_name == "LegalShield")
            .unwrap_or_else(|| panic!("LegalShield bucket"));
        assert_eq!(legal_shield.state, None);

        let other = matter_counts(&pg, b.firm_id, day(11, 1), day(11, 30))
            .await
            .unwrap_or_else(|error| panic!("firm b counts: {error}"));
        assert!(other.is_empty(), "firm B has no plan matters in the window");
        assert_ne!(other.iter().map(|row| row.legal_plan_id).next(), Some(arag_a));
    }

    #[tokio::test]
    async fn sla_misses_come_from_the_firm_s_own_budgets() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let a = seed_firm(&pg).await;
        let b = seed_firm(&pg).await;
        let arag_a = plan(&pg, a.firm_id, "ARAG").await;
        let arag_b = plan(&pg, b.firm_id, "ARAG").await;

        // Same-day acceptance is never a miss, whatever the budget.
        let _ = matter(&pg, &a, "plan-ok", Some(arag_a), Some("document_review"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 3)), None).await;
        // One day over a 24-hour budget is one miss.
        let late = matter(&pg, &a, "plan-late", Some(arag_a), Some("document_review"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 5)), None).await;
        // 30 days acceptance-to-close over a 7-day budget is one miss.
        let slow = matter(&pg, &a, "plan-slow", Some(arag_a), Some("letter"), Some("ny"),
            Some(day(11, 1)), Some(day(11, 2)), Some(day(11, 30))).await;

        let acceptance = acceptance_sla_misses(&pg, a.firm_id, day(11, 1), day(11, 30))
            .await
            .unwrap_or_else(|error| panic!("acceptance misses: {error}"));
        assert_eq!(acceptance.len(), 1, "same-day acceptance never misses");
        assert_eq!(acceptance[0].project_id, late);
        assert_eq!(acceptance[0].elapsed_hours, 48);
        assert_eq!(acceptance[0].acceptance_sla_hours, 24);

        let turnaround = turnaround_sla_misses(&pg, a.firm_id, day(11, 1), day(11, 30))
            .await
            .unwrap_or_else(|error| panic!("turnaround misses: {error}"));
        assert_eq!(turnaround.len(), 1);
        assert_eq!(turnaround[0].project_id, slow);
        assert_eq!(turnaround[0].elapsed_days, 28);
        assert_eq!(turnaround[0].turnaround_sla_days, 30 - 23, "the seed budget is 7 days");

        // Firm B accepts its matter just as late — its miss must not
        // appear in Firm A's list, and vice versa.
        let _ = matter(&pg, &b, "plan-b-late", Some(arag_b), Some("document_review"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 5)), None).await;
        assert_eq!(
            acceptance_sla_misses(&pg, a.firm_id, day(11, 1), day(11, 30))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            acceptance_sla_misses(&pg, b.firm_id, day(11, 1), day(11, 30))
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            turnaround_sla_misses(&pg, b.firm_id, day(11, 1), day(11, 30))
                .await
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn minutes_by_stage_follow_the_firm_s_stage_positions() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let a = seed_firm(&pg).await;
        let b = seed_firm(&pg).await;
        let arag_a = plan(&pg, a.firm_id, "ARAG").await;
        let hearing = stage(&pg, a.firm_id, "hearing", 5).await;
        let draft = stage(&pg, a.firm_id, "draft", 2).await;

        let matter_a = matter(&pg, &a, "plan-a1", Some(arag_a), Some("document_review"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 3)), None).await;
        let _ = entry(&pg, matter_a, a.attorney, 30, 20_000, Some(hearing)).await;
        let _ = entry(&pg, matter_a, a.attorney, 20, 20_000, Some(hearing)).await;
        let _ = entry(&pg, matter_a, a.attorney, 40, 20_000, Some(draft)).await;
        let _ = entry(&pg, matter_a, a.attorney, 15, 20_000, None).await;

        let by_stage = minutes_by_stage_for_project(&pg, a.firm_id, matter_a)
            .await
            .unwrap_or_else(|error| panic!("minutes by stage: {error}"));
        assert_eq!(by_stage.len(), 3, "draft, hearing, and the unstaged bucket");
        assert_eq!(by_stage[0].stage_id, Some(draft));
        assert_eq!(by_stage[0].stage_label.as_deref(), Some("draft"));
        assert_eq!((by_stage[0].entry_count, by_stage[0].minutes), (1, 40));
        assert_eq!(by_stage[1].stage_id, Some(hearing));
        assert_eq!((by_stage[1].entry_count, by_stage[1].minutes), (2, 50));
        assert_eq!(by_stage[2].stage_id, None, "the unstaged bucket comes last");
        assert_eq!((by_stage[2].entry_count, by_stage[2].minutes), (1, 15));

        let by_plan = minutes_by_stage_for_plan(&pg, a.firm_id, arag_a)
            .await
            .unwrap_or_else(|error| panic!("plan minutes: {error}"));
        assert_eq!(by_plan, by_stage, "one matter is the whole plan so far");

        // A second Firm's matter of the same shape answers nothing here.
        let _ = matter(&pg, &b, "plan-b1", None, None, None, Some(day(11, 3)), None, None).await;
        assert!(minutes_by_stage_for_project(&pg, a.firm_id, b_attorney_matter(
            &pg, &b, &arag_a
        ).await)
        .await
        .unwrap()
        .is_empty());
    }

    async fn b_attorney_matter(pg: &PostgresDb, fixture_b: &FirmFixture, arag_b: &Uuid) -> Uuid {
        matter(pg, fixture_b, "plan-b2", Some(*arag_b), Some("letter"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 3)), None).await
    }

    #[tokio::test]
    async fn economics_resolve_the_effective_fee_and_the_entry_pieces() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let a = seed_firm(&pg).await;
        let b = seed_firm(&pg).await;
        let arag_a = plan(&pg, a.firm_id, "ARAG").await;
        let arag_b = plan(&pg, b.firm_id, "ARAG").await;

        // Two generations of the same fee: a matter priced on or after
        // June 1 reads the newer fee.
        let _old = schedule(&pg, a.firm_id, arag_a, &PlanMatterType::DocumentReview, 15_000, day(1, 1)).await;
        let _new = schedule(&pg, a.firm_id, arag_a, &PlanMatterType::DocumentReview, 17_500, day(6, 1)).await;
        // Firm B's own schedule for the same plan name and type — never
        // Firm A's answer.
        let _b_fee = schedule(&pg, b.firm_id, arag_b, &PlanMatterType::DocumentReview, 100, day(1, 1)).await;

        let priced = matter(&pg, &a, "plan-priced", Some(arag_a), Some("document_review"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 4)), None).await;
        let _ = entry(&pg, priced, a.attorney, 90, 25_000, None).await;
        let _ = entry(&pg, priced, a.attorney, 30, 25_000, None).await;
        // The unpriced matter: no schedule row for the type.
        let unpriced = matter(&pg, &a, "plan-unpriced", Some(arag_a), Some("letter"), Some("ny"),
            Some(day(11, 3)), None, None).await;
        let _ = entry(&pg, unpriced, a.attorney, 60, 25_000, None).await;

        let economics = matter_economics(&pg, a.firm_id, day(11, 1), day(11, 30))
            .await
            .unwrap_or_else(|error| panic!("economics: {error}"));
        assert_eq!(economics.len(), 2);
        let priced_row = economics
            .iter()
            .find(|row| row.project_id == priced)
            .unwrap_or_else(|| panic!("priced matter"));
        assert_eq!(priced_row.plan_fee_cents, Some(17_500), "the newer effective fee");
        assert_eq!(priced_row.time_cost_cents, 50_000, "37_500 + 12_500, the entry pieces");
        assert_eq!(priced_row.margin_cents, Some(17_500 - 50_000));
        assert_eq!(priced_row.margin_cents_per_hour, Some(-16_250));
        assert_eq!(priced_row.matter_type, PlanMatterType::DocumentReview);

        let unpriced_row = economics
            .iter()
            .find(|row| row.project_id == unpriced)
            .unwrap_or_else(|| panic!("unpriced matter"));
        assert_eq!(unpriced_row.plan_fee_cents, None);
        assert_eq!(unpriced_row.margin_cents, None);
        assert_eq!(unpriced_row.time_cost_cents, 25_000);

        let rollup = margin_by_matter_type(&pg, a.firm_id, day(11, 1), day(11, 30))
            .await
            .unwrap_or_else(|error| panic!("rollup: {error}"));
        assert_eq!(rollup.len(), 2);
        let letters = rollup
            .iter()
            .find(|row| row.matter_type == PlanMatterType::Letter)
            .unwrap_or_else(|| panic!("letter rollup"));
        assert_eq!((letters.matter_count, letters.unpriced_matters), (1, 1));
        assert_eq!(letters.plan_fee_cents, 0);
        assert_eq!(letters.time_cost_cents, 25_000);
        let reviews = rollup
            .iter()
            .find(|row| row.matter_type == PlanMatterType::DocumentReview)
            .unwrap_or_else(|| panic!("document review rollup"));
        assert_eq!((reviews.matter_count, reviews.unpriced_matters), (1, 0));
        assert_eq!(reviews.margin_cents, -32_500);
        assert_eq!(reviews.margin_cents_per_hour, -16_250);

        // Firm B's matter — same dates, same type, its own cheap fee —
        // never appears in Firm A's economics.
        let _ = matter(&pg, &b, "plan-b-priced", Some(arag_b), Some("document_review"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 4)), None).await;
        let firm_a_codes: Vec<String> = matter_economics(&pg, a.firm_id, day(11, 1), day(11, 30))
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.code)
            .collect();
        assert_eq!(firm_a_codes, vec!["plan-priced".to_owned(), "plan-unpriced".to_owned()]);
    }

    #[tokio::test]
    async fn template_effect_splits_the_cohorts_by_first_use() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let a = seed_firm(&pg).await;
        let b = seed_firm(&pg).await;
        let arag_a = plan(&pg, a.firm_id, "ARAG").await;
        let arag_b = plan(&pg, b.firm_id, "ARAG").await;
        let draft = stage(&pg, a.firm_id, "draft", 1).await;
        let review = stage(&pg, a.firm_id, "review", 2).await;
        let scribe = person(&pg, "Scribe").await;

        // Two versions of the demand-letter code in Firm A's library:
        // first existence is the oldest row's insert, February 1.
        let _v1 = template_version(&pg, "demand-letter", Some(a.firm_id), stamp(500), false).await;
        let _v2 = template_version(&pg, "demand-letter", Some(a.firm_id), stamp(501), true).await;

        let with_one = matter(&pg, &a, "plan-with-1", Some(arag_a), Some("letter"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 3)), None).await;
        let _ = notation(&pg, _v1, scribe, with_one, stamp(0)).await;
        let _ = entry(&pg, with_one, a.attorney, 60, 20_000, Some(draft)).await;

        let with_two = matter(&pg, &a, "plan-with-2", Some(arag_a), Some("letter"), Some("ny"),
            Some(day(11, 4)), Some(day(11, 4)), None).await;
        let _ = notation(&pg, _v2, scribe, with_two, stamp(0)).await;
        let _ = entry(&pg, with_two, a.attorney, 40, 20_000, Some(draft)).await;
        let _ = entry(&pg, with_two, a.attorney, 20, 20_000, Some(review)).await;

        // Uses a February 1 template before it existed — the guard keeps
        // it in the "before" cohort.
        let guard = matter(&pg, &a, "plan-guard", Some(arag_a), Some("letter"), Some("ny"),
            Some(day(11, 5)), Some(day(11, 5)), None).await;
        let early = template_version(&pg, "demand-letter", Some(a.firm_id), stamp(600), false).await;
        let _ = notation(&pg, early, scribe, guard, stamp(1)).await;
        let _ = entry(&pg, guard, a.attorney, 100, 20_000, Some(draft)).await;

        let plain = matter(&pg, &a, "plan-plain", Some(arag_a), Some("letter"), Some("ny"),
            Some(day(11, 6)), Some(day(11, 6)), None).await;
        let _ = entry(&pg, plain, a.attorney, 100, 20_000, Some(draft)).await;

        let other_type = matter(&pg, &a, "plan-other", Some(arag_a), Some("document_review"), Some("ny"),
            Some(day(11, 7)), Some(day(11, 7)), None).await;
        let _ = notation(&pg, _v2, scribe, other_type, stamp(0)).await;
        let _ = entry(&pg, other_type, a.attorney, 10, 20_000, Some(draft)).await;

        let effects = template_effect(&pg, a.firm_id, &["demand-letter".to_owned()])
            .await
            .unwrap_or_else(|error| panic!("template effect: {error}"));
        let letter_draft = effects
            .iter()
            .find(|row| {
                row.matter_type == PlanMatterType::Letter
                    && row.stage_id == Some(draft)
            })
            .unwrap_or_else(|| panic!("letter draft row"));
        assert_eq!(letter_draft.with_matters, 2);
        assert_eq!(letter_draft.with_minutes, 100);
        assert_eq!(letter_draft.before_matters, 2);
        assert_eq!(letter_draft.before_minutes, 200);
        assert_eq!(letter_draft.with_average_minutes(), 50);
        assert_eq!(letter_draft.before_average_minutes(), 100);

        let letter_review = effects
            .iter()
            .find(|row| row.stage_id == Some(review))
            .unwrap_or_else(|| panic!("letter review row"));
        assert_eq!((letter_review.with_matters, letter_review.with_minutes), (1, 20));
        assert_eq!((letter_review.before_matters, letter_review.before_minutes), (0, 0));

        let review_draft = effects
            .iter()
            .find(|row| row.matter_type == PlanMatterType::DocumentReview && row.stage_id == Some(draft))
            .unwrap_or_else(|| panic!("document review draft row"));
        assert_eq!((review_draft.with_matters, review_draft.before_matters), (1, 0));

        // Firm B holds its own demand-letter library row inserted January
        // 1 and a "with" matter — none of it shifts Firm A's cohorts.
        let _b_library = template_version(&pg, "demand-letter", Some(b.firm_id), stamp(0), true).await;
        let _ = template_version(&pg, "demand-letter", Some(b.firm_id), stamp(100), false).await;
        let b_matter = matter(&pg, &b, "plan-b-with", Some(arag_b), Some("letter"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 3)), None).await;
        let _ = notation(&pg, _b_library, scribe, b_matter, stamp(200)).await;
        let _ = entry(&pg, b_matter, b.attorney, 999, 20_000, Some(draft)).await;
        let firm_a_rows: Vec<(String, i64, i64)> = template_effect(&pg, a.firm_id, &["demand-letter".to_owned()])
            .await
            .unwrap()
            .iter()
            .filter(|row| row.stage_id == Some(draft) && row.matter_type == PlanMatterType::Letter)
            .map(|row| (row.template_code.clone(), row.with_minutes, row.before_minutes))
            .collect();
        assert_eq!(firm_a_rows, vec![("demand-letter".to_owned(), 100, 200)]);

        // A code with no template rows in scope answers nothing.
        assert!(template_effect(&pg, a.firm_id, &["missing-code".to_owned()])
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn firm_report_resolves_the_view_plan_metrics_capability() {
        let Some((options, database)) = fresh_schema().await else {
            return;
        };
        let pg = connect(&options, &database).await;
        let surreal = mem_surreal().await;
        let entity_id = seed_entity(&surreal).await;

        let owner = persons::create(
            &surreal,
            &NewPerson::with_role("Synthetic Owner", "owner-metrics@example.com", Role::Owner),
        )
        .await
        .unwrap_or_else(|error| panic!("owner: {error}"));
        let admin = persons::create(
            &surreal,
            &NewPerson::with_role("Synthetic Admin", "admin-metrics@example.com", Role::Admin),
        )
        .await
        .unwrap_or_else(|error| panic!("admin: {error}"));
        let lawyer = persons::create(
            &surreal,
            &NewPerson::with_role("Synthetic Lawyer", "lawyer-metrics@example.com", Role::Lawyer),
        )
        .await
        .unwrap_or_else(|error| panic!("lawyer: {error}"));

        let created = firms::create(
            &surreal,
            &NewFirm {
                name: "Synthetic Metrics Firm".to_owned(),
                status: "active".to_owned(),
                entity_id,
                admin_dri_person_id: admin.id,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("firm: {error}"));
        firms::add_membership(
            &surreal,
            &NewPersonFirmRole {
                person_id: admin.id,
                firm_id: created.id,
                membership: FirmMembership::Admin,
                is_dri: true,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("admin membership: {error}"));
        firms::add_membership(
            &surreal,
            &NewPersonFirmRole {
                person_id: lawyer.id,
                firm_id: created.id,
                membership: FirmMembership::Lawyer,
                is_dri: false,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("lawyer membership: {error}"));

        // The same Firm mirrored to PostgreSQL, where the plan data lives.
        let fixture = FirmFixture {
            firm_id: created.id,
            entity_id: entity(&pg).await,
            attorney: person(&pg, "Attorney").await,
        };
        sqlx::query("INSERT INTO firm (id, name, slug, status) VALUES ($1, 'Synthetic Metrics Firm', $2, 'active')")
            .bind(created.id)
            .bind(format!("pm-{}", Uuid::new_v4().simple()))
            .execute(&pg.0)
            .await
            .unwrap_or_else(|error| panic!("mirrored firm row: {error}"));
        let arag = plan(&pg, created.id, "ARAG").await;
        let matter_a = matter(&pg, &fixture, "plan-a1", Some(arag), Some("document_review"), Some("ny"),
            Some(day(11, 3)), Some(day(11, 4)), None).await;
        let _ = entry(&pg, matter_a, fixture.attorney, 45, 20_000, None).await;

        let window = (day(11, 1), day(11, 30));
        let admin_report = firm_report(&pg, &surreal, admin.id, created.id, window.0, window.1)
            .await
            .unwrap_or_else(|error| panic!("admin report: {error}"));
        assert_eq!(admin_report.counts.len(), 1);
        assert_eq!(admin_report.counts[0].plan_name, "ARAG");
        assert_eq!(admin_report.economics.len(), 1);
        assert_eq!(admin_report.economics[0].minutes, 45);
        assert_eq!(admin_report.acceptance_misses.len(), 0);
        assert_eq!(admin_report.turnaround_misses.len(), 0);

        // The Owner is system-wide and reads the same report.
        let owner_report = firm_report(&pg, &surreal, owner.id, created.id, window.0, window.1)
            .await
            .unwrap_or_else(|error| panic!("owner report: {error}"));
        assert_eq!(owner_report, admin_report);

        // The Lawyer is refused their own Firm's economics.
        assert!(
            matches!(
                firm_report(&pg, &surreal, lawyer.id, created.id, window.0, window.1).await,
                Err(PlanMetricsError::Denied)
            ),
            "a Lawyer's ViewPlanMetrics resolution refuses"
        );
    }
}
