//! The Firm's operational rollups for the [CEO Digest](../docs/glossary/ceo-digest.md)
//! and its [Daily Alert](../docs/glossary/daily-alert.md) companion (ENG-1264):
//! one Firm-keyed query per figure, each journaled by the workflow that reads
//! it, every row filtered to one `firm_id` so a second Firm's data never
//! appears in another Firm's brief.
//!
//! The queries are PostgreSQL-first: `time_entry`, `stage_event`,
//! `project_deadlines`, and the inbox mirror are PostgreSQL tables, and the
//! plan columns on `project` arrive through the tenancy mirror. The three
//! figures whose write authority is still SurrealDB — Xero-backed invoices and
//! the trust ledger inside the money fold, docket entries inside
//! last-activity, and `statutory_deadline` rows beside the deadline scan —
//! read through the store's existing accessors and fold in Rust, exactly the
//! way [`crate::matter_financials::for_project`] already pairs the lanes.
//!
//! Money follows the [`crate::finance_aggregate`] rule: integer cents, a
//! group total sums the per-matter rounded pieces rather than re-splitting
//! raw totals. The needs-you rule is a pure helper,
//! [`business_days_between`], so the two-business-day threshold is unit-tested
//! without a database; weekends count off, and no court-holiday calendar is
//! applied — `store::courts` carries a court registry, not a holiday calendar.
//!
//! The durable workflows call these Firm-keyed queries directly, as the system
//! actor; an actor-facing caller would route through
//! [`crate::firm_capability`] first, exactly as [`crate::plan_metrics`]
//! requires. See the digest's section notes for what is deliberately absent:
//! funnel and referral sections have no data source yet and ship omitted.
//!
//! [CEO Digest]: ../docs/glossary/ceo-digest.md
//! [Daily Alert]: ../docs/glossary/daily-alert.md

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::postgres::PostgresDb;

/// Parses a stored RFC 3339 instant into UTC. Every stored value was written
/// by store accessors, so a value that fails to parse is a data problem worth
/// surfacing, not skipping.
fn parse_instant(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|parsed| parsed.with_timezone(&Utc))
}

#[derive(Debug, thiserror::Error)]
pub enum FirmDigestError {
    #[error("invalid firm digest request: {0}")]
    Invalid(&'static str),
    #[error("the PostgreSQL rollup failed")]
    Database(#[from] sqlx::Error),
    #[error("a SurrealDB-authored read failed: {0}")]
    Surreal(String),
}

/// The matter lines a Firm's projects split into. The plan columns separate
/// the [Legal Plan](crate::legal_plans::LegalPlan) line from everything else;
/// a commercial-litigation versus pro-bono distinction has no source column
/// yet, so those two lines arrive with their data rather than splitting on a
/// guess (the digest's omit-what-has-no-source rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum MatterLine {
    /// A legal-plan panel matter: `legal_plan_id` is set.
    Plan,
    /// Every other matter: every plan column unset.
    Other,
}

impl MatterLine {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Other => "other",
        }
    }

    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "plan" => Some(Self::Plan),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

/// One open matter, as the rollups' shared selector sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenMatter {
    pub project_id: Uuid,
    pub code: String,
    pub line: MatterLine,
}

/// Open matters per line — the firm-scoped replacement for
/// `store::projects::matter_open_pitch_counts`, which is hard-scoped to brand
/// `"neon"`. A line absent from the result has no open matters.
///
/// # Errors
///
/// [`FirmDigestError::Database`] on a database failure.
pub async fn open_matters_by_line(
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<BTreeMap<MatterLine, i64>, FirmDigestError> {
    let rows = sqlx::query(
        "SELECT CASE WHEN legal_plan_id IS NOT NULL THEN 'plan' ELSE 'other' END AS line, \
         COUNT(*)::bigint AS open_matters \
         FROM project WHERE firm_id = $1 AND status = 'open' GROUP BY 1 ORDER BY 1",
    )
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok((
            MatterLine::parse(&row.try_get::<String, _>("line")?)
                .ok_or(FirmDigestError::Invalid("matter line"))?,
            row.try_get::<i64, _>("open_matters")?,
        ))
    })
    .collect::<Result<BTreeMap<_, _>, FirmDigestError>>()?;
    Ok(rows)
}

/// Every open matter of the Firm, ordered by code for stable rendering.
///
/// # Errors
///
/// [`FirmDigestError::Database`] on a database failure.
pub async fn open_matters(
    pg: &PostgresDb,
    firm_id: Uuid,
) -> Result<Vec<OpenMatter>, FirmDigestError> {
    let rows = sqlx::query(
        "SELECT id, code, CASE WHEN legal_plan_id IS NOT NULL THEN 'plan' ELSE 'other' END AS line \
         FROM project WHERE firm_id = $1 AND status = 'open' ORDER BY code, id",
    )
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(OpenMatter {
            project_id: row.try_get("id")?,
            code: row.try_get("code")?,
            line: MatterLine::parse(&row.try_get::<String, _>("line")?)
                .ok_or(FirmDigestError::Invalid("matter line"))?,
        })
    })
    .collect::<Result<Vec<_>, FirmDigestError>>()?;
    Ok(rows)
}

/// Work in progress, outstanding balance, and trust held, per line. Each
/// matter is folded through [`crate::matter_financials::for_project`] and
/// [`crate::trust::position_for_project`]; a line's total sums the per-matter
/// pieces — the `finance_aggregate` rule — and trust held is the folded
/// position's `held()`.
///
/// # Errors
///
/// [`FirmDigestError::Database`] or [`FirmDigestError::Surreal`] when a
/// per-matter read fails.
pub async fn line_money(
    pg: &PostgresDb,
    surreal: &crate::surreal::SurrealDb,
    firm_id: Uuid,
) -> Result<BTreeMap<MatterLine, LineMoney>, FirmDigestError> {
    let mut totals = BTreeMap::from([
        (MatterLine::Plan, LineMoney::default()),
        (MatterLine::Other, LineMoney::default()),
    ]);
    for matter in open_matters(pg, firm_id).await? {
        let financials = crate::matter_financials::for_project(surreal, pg, matter.project_id)
            .await
            .map_err(|error| FirmDigestError::Surreal(error.to_string()))?;
        let trust = crate::trust::position_for_project(surreal, matter.project_id)
            .await
            .map_err(FirmDigestError::Surreal)?;
        let line = totals.entry(matter.line).or_default();
        line.wip_cents += financials.wip_cents;
        line.outstanding_cents += financials.outstanding_cents;
        line.trust_held_cents += trust.held_cents();
    }
    Ok(totals)
}

/// The money rollup for one line: totals of the per-matter rounded pieces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineMoney {
    pub wip_cents: i64,
    pub outstanding_cents: i64,
    pub trust_held_cents: i64,
}

/// Where a deadline row comes from: the mirrored Project Deadline table or a
/// Statutory Deadline recorded by workflow replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeadlineSource {
    Deadline,
    Statutory,
}

/// One deadline the Firm must watch, in its raw due value — the locked
/// "due means the raw `due_at`" decision; the `internal_lead_days` refinement
/// is future work, noted rather than half-built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadlineItem {
    pub project_id: Uuid,
    pub code: String,
    /// The deadline's title, the statutory kind when the row has none.
    pub label: String,
    /// The raw due value: `project_deadlines.due_at` (TIMESTAMPTZ) or
    /// `statutory_deadline.due_on` (DATE), each rendered UTC.
    pub due: String,
    pub source: DeadlineSource,
}

/// Due soon (inside the horizon) and missed (still open, already past), from
/// the `project_deadlines` PostgreSQL mirror plus `statutory_deadline` rows
/// (date-only) read through the store's per-project accessor. Cancelled and
/// completed rows never appear, and only open matters are scanned.
///
/// # Errors
///
/// [`FirmDigestError::Database`] or [`FirmDigestError::Surreal`] on a
/// database failure.
pub async fn deadlines(
    pg: &PostgresDb,
    surreal: &crate::surreal::SurrealDb,
    firm_id: Uuid,
    now: DateTime<Utc>,
    horizon_days: u32,
) -> Result<FirmDeadlines, FirmDigestError> {
    let horizon_end = now
        .date_naive()
        .checked_add_days(chrono::Days::new(u64::from(horizon_days)))
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .map(|time| time.and_utc())
        .ok_or(FirmDigestError::Invalid("horizon"))?;
    let rows = sqlx::query(
        "SELECT d.project_id, p.code, d.title, d.due_at \
         FROM project_deadlines d JOIN project p ON p.id = d.project_id \
         WHERE p.firm_id = $1 AND p.status = 'open' \
           AND d.status = 'open' AND d.due_at < $2 \
         ORDER BY d.due_at, d.id",
    )
    .bind(firm_id)
    .bind(horizon_end)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(DeadlineItem {
            project_id: row.try_get("project_id")?,
            code: row.try_get("code")?,
            label: row
                .try_get::<Option<String>, _>("title")?
                .unwrap_or_else(|| "Deadline".to_owned()),
            due: row.try_get::<DateTime<Utc>, _>("due_at")?.to_rfc3339(),
            source: DeadlineSource::Deadline,
        })
    })
    .collect::<Result<Vec<_>, FirmDigestError>>()?;
    let mut deadlines = FirmDeadlines::default();
    for row in rows {
        let due = parse_instant(&row.due).ok_or(FirmDigestError::Invalid("due_at"))?;
        if due < now {
            deadlines.missed.push(row);
        } else {
            deadlines.due_soon.push(row);
        }
    }
    append_statutory(pg, surreal, firm_id, now, horizon_days, &mut deadlines).await?;
    Ok(deadlines)
}

/// The deadline scan's output: due-soon and missed, each ordered by due date.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirmDeadlines {
    pub due_soon: Vec<DeadlineItem>,
    pub missed: Vec<DeadlineItem>,
}

/// Statutory rows have no PostgreSQL mirror, so each open matter is read
/// through [`crate::statutory_deadlines::by_project`] and folded here.
async fn append_statutory(
    pg: &PostgresDb,
    surreal: &crate::surreal::SurrealDb,
    firm_id: Uuid,
    now: DateTime<Utc>,
    horizon_days: u32,
    deadlines: &mut FirmDeadlines,
) -> Result<(), FirmDigestError> {
    let horizon_end = now
        .date_naive()
        .checked_add_days(chrono::Days::new(u64::from(horizon_days)))
        .ok_or(FirmDigestError::Invalid("horizon"))?;
    for matter in open_matters(pg, firm_id).await? {
        for deadline in crate::statutory_deadlines::by_project(surreal, matter.project_id)
            .await
            .map_err(|error| FirmDigestError::Surreal(error.to_string()))?
        {
            if deadline.status != crate::statutory_deadlines::STATUS_OPEN {
                continue;
            }
            if deadline.due_on > horizon_end {
                continue;
            }
            let item = DeadlineItem {
                project_id: matter.project_id,
                code: matter.code.clone(),
                label: deadline.kind.clone(),
                due: deadline.due_on.format("%Y-%m-%d").to_string(),
                source: DeadlineSource::Statutory,
            };
            if deadline.due_on < now.date_naive() {
                deadlines.missed.push(item);
            } else {
                deadlines.due_soon.push(item);
            }
        }
    }
    Ok(())
}

/// One conversation the Firm owes a reply on: an inbound message with no later
/// outbound on the same conversation, older than two business days. The
/// subject and counterparty name are the conversation's, not the message
/// body — the alert names the thread, never quotes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NeedsYouItem {
    pub project_id: Uuid,
    pub conversation_id: Uuid,
    pub external_name: Option<String>,
    pub subject: Option<String>,
    /// The newest unanswered inbound's occurred_at.
    pub last_inbound: DateTime<Utc>,
}

/// Needs-you across the Firm's project-attributed conversations: inbound with
/// no later outbound, older than two business days ([`business_days_between`],
/// weekends off — no holiday calendar is applied). Read from the inbox
/// mirror's PostgreSQL lane, the same lane
/// [`crate::postgres::inbox::unread_count_for_project_on`] counts; a
/// deployment whose conversations were never mirrored answers nothing rather
/// than reading the write authority through a second, firm-unfiltered query.
/// The last-inbound/last-outbound aggregation picks each side's maximum by
/// text comparison, which the mirror's uniform RFC 3339 UTC rendering makes
/// exact; the picked values are parsed, not trusted.
///
/// # Errors
///
/// [`FirmDigestError::Database`] on a database failure,
/// [`FirmDigestError::Invalid`] when a stored timestamp does not parse.
pub async fn needs_you(
    pg: &PostgresDb,
    firm_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Vec<NeedsYouItem>, FirmDigestError> {
    let rows = sqlx::query(
        "SELECT c.id AS conversation_id, c.project_id, c.subject, c.external_name, \
         MAX(m.occurred_at) FILTER (WHERE m.direction = 'inbound') AS last_inbound, \
         MAX(m.occurred_at) FILTER (WHERE m.direction = 'outbound') AS last_outbound \
         FROM inbox_conversation c \
         JOIN message m ON m.conversation_id = c.id AND m.purpose = 'matter' \
         WHERE c.firm_id = $1 AND c.project_id IS NOT NULL \
         GROUP BY c.id, c.project_id, c.subject, c.external_name \
         ORDER BY c.id",
    )
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| -> Result<Option<NeedsYouItem>, FirmDigestError> {
        let Some(inbound) = row.try_get::<Option<String>, _>("last_inbound")? else {
            return Ok(None);
        };
        let last_inbound =
            parse_instant(&inbound).ok_or(FirmDigestError::Invalid("message.occurred_at"))?;
        if let Some(outbound) = row.try_get::<Option<String>, _>("last_outbound")? {
            let last_outbound =
                parse_instant(&outbound).ok_or(FirmDigestError::Invalid("message.occurred_at"))?;
            if last_outbound >= last_inbound {
                return Ok(None);
            }
        }
        if business_days_between(last_inbound.date_naive(), now.date_naive()) <= 2 {
            return Ok(None);
        }
        Ok(Some(NeedsYouItem {
            project_id: row.try_get("project_id")?,
            conversation_id: row.try_get("conversation_id")?,
            external_name: row.try_get("external_name")?,
            subject: row.try_get("subject")?,
            last_inbound,
        }))
    })
    .collect::<Result<Vec<_>, FirmDigestError>>()?
    .into_iter()
    .flatten()
    .collect();
    Ok(rows)
}

/// Weekdays in the half-open interval `(from, to]` — the business days that
/// have elapsed since `from`. Weekend days count off; no holiday calendar is
/// applied, because `store::courts` is a court registry, not a holiday
/// calendar. A `to` on or before `from` counts nothing.
#[must_use]
pub fn business_days_between(from: NaiveDate, to: NaiveDate) -> u32 {
    let mut count = 0;
    let mut day = from;
    while day < to {
        day = match day.succ_opt() {
            Some(next) => next,
            None => break,
        };
        if day.weekday().num_days_from_monday() < 5 {
            count += 1;
        }
    }
    count
}

/// Attorney minutes this week, per attorney, per line, per Case Stage — the
/// same stage vocabulary [`crate::plan_metrics::minutes_by_stage_for_plan`]
/// attributes, split additionally by attorney and matter line. The unstaged
/// bucket reads as a stage with no label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttorneyStageMinutes {
    pub attorney_id: Uuid,
    /// The attorney's display name from the mirrored `person` row; `None`
    /// when the mirror has not caught up.
    pub attorney_name: Option<String>,
    pub line: MatterLine,
    pub stage_id: Option<Uuid>,
    pub stage_label: Option<String>,
    pub minutes: i64,
}

/// Minutes by attorney, line, and stage over `[from, to]`.
///
/// # Errors
///
/// [`FirmDigestError::Database`] on a database failure.
pub async fn minutes_by_attorney_and_stage(
    pg: &PostgresDb,
    firm_id: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<AttorneyStageMinutes>, FirmDigestError> {
    let rows = sqlx::query(
        "SELECT te.attorney_id, per.name AS attorney_name, \
         CASE WHEN p.legal_plan_id IS NOT NULL THEN 'plan' ELSE 'other' END AS line, \
         te.stage AS stage_id, sd.label AS stage_label, SUM(te.minutes)::bigint AS minutes \
         FROM time_entry te \
         JOIN project p ON p.id = te.project_id \
         LEFT JOIN person per ON per.id = te.attorney_id \
         LEFT JOIN stage_definition sd ON sd.id = te.stage \
         WHERE p.firm_id = $1 AND te.performed_on BETWEEN $2 AND $3 \
         GROUP BY te.attorney_id, per.name, \
                  CASE WHEN p.legal_plan_id IS NOT NULL THEN 'plan' ELSE 'other' END, \
                  te.stage, sd.label, sd.position \
         ORDER BY per.name NULLS LAST, 3, sd.position NULLS LAST",
    )
    .bind(firm_id)
    .bind(from)
    .bind(to)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok(AttorneyStageMinutes {
            attorney_id: row.try_get("attorney_id")?,
            attorney_name: row.try_get("attorney_name")?,
            line: MatterLine::parse(&row.try_get::<String, _>("line")?)
                .ok_or(FirmDigestError::Invalid("matter line"))?,
            stage_id: row.try_get("stage_id")?,
            stage_label: row.try_get("stage_label")?,
            minutes: row.try_get("minutes")?,
        })
    })
    .collect::<Result<Vec<_>, FirmDigestError>>()?;
    Ok(rows)
}

/// The newest activity a matter shows, per the digest's stated definition: the
/// max of the newest [`crate::case_stages::StageEvent`] `occurred_at`, the
/// newest [`crate::time_entries::TimeEntry`] `performed_on` (read as that
/// date's midnight UTC), and the newest [`crate::cases::DocketEntry`] date
/// (`filed_or_served_on`, else `scheduled_on`). No first-class column exists;
/// the digest computes the max of the three and says so in its own section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatterLastActivity {
    pub project_id: Uuid,
    pub code: String,
    pub line: MatterLine,
    /// `None` when the matter shows none of the three yet.
    pub last_activity: Option<DateTime<Utc>>,
}

/// Last activity per open matter of the Firm.
///
/// # Errors
///
/// [`FirmDigestError::Database`] or [`FirmDigestError::Surreal`] on a
/// database failure.
pub async fn last_activity(
    pg: &PostgresDb,
    surreal: &crate::surreal::SurrealDb,
    firm_id: Uuid,
) -> Result<Vec<MatterLastActivity>, FirmDigestError> {
    let rows = sqlx::query(
        "SELECT p.id AS project_id, p.code, \
         CASE WHEN p.legal_plan_id IS NOT NULL THEN 'plan' ELSE 'other' END AS line, \
         (SELECT MAX(se.occurred_at) FROM stage_event se WHERE se.project_id = p.id) \
            AS last_stage_at, \
         (SELECT MAX(te.performed_on) FROM time_entry te WHERE te.project_id = p.id) \
            AS last_entry_on \
         FROM project p WHERE p.firm_id = $1 AND p.status = 'open' ORDER BY p.code, p.id",
    )
    .bind(firm_id)
    .fetch_all(&pg.0)
    .await?
    .iter()
    .map(|row| {
        Ok((
            MatterLastActivity {
                project_id: row.try_get("project_id")?,
                code: row.try_get("code")?,
                line: MatterLine::parse(&row.try_get::<String, _>("line")?)
                    .ok_or(FirmDigestError::Invalid("matter line"))?,
                last_activity: None,
            },
            row.try_get::<Option<DateTime<Utc>>, _>("last_stage_at")?,
            row.try_get::<Option<NaiveDate>, _>("last_entry_on")?,
        ))
    })
    .collect::<Result<Vec<_>, FirmDigestError>>()?;
    let mut out = Vec::with_capacity(rows.len());
    for ((mut item, last_stage_at, last_entry_on), ()) in rows.into_iter().map(|row| (row, ())) {
        let mut newest = last_stage_at;
        if let Some(day) = last_entry_on.and_then(|day| day.and_hms_opt(0, 0, 0)) {
            let entry_at = day.and_utc();
            if newest.is_none_or(|current| entry_at > current) {
                newest = Some(entry_at);
            }
        }
        if let Some(docket_at) = docket_activity(surreal, item.project_id)
            .await
            .unwrap_or(None)
        {
            if newest.is_none_or(|current| docket_at > current) {
                newest = Some(docket_at);
            }
        }
        item.last_activity = newest;
        out.push(item);
    }
    Ok(out)
}

/// The newest DocketEntry date on one matter, `None` when it has none.
async fn docket_activity(
    surreal: &crate::surreal::SurrealDb,
    project_id: Uuid,
) -> Result<Option<DateTime<Utc>>, FirmDigestError> {
    let newest = crate::cases::docket_for_project(surreal, project_id)
        .await
        .map_err(|error| FirmDigestError::Surreal(error.to_string()))?
        .into_iter()
        .filter_map(|entry| {
            entry
                .filed_or_served_on
                .as_deref()
                .and_then(parse_instant)
                .or(entry.scheduled_on)
        })
        .max();
    Ok(newest)
}

#[cfg(test)]
mod tests {
    use super::{business_days_between, MatterLine};
    use chrono::NaiveDate;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap_or_else(|| panic!("valid date"))
    }

    #[test]
    fn business_days_count_weekdays_after_the_from_date() {
        // Monday → Thursday counts Tuesday, Wednesday, Thursday.
        assert_eq!(business_days_between(day(2026, 11, 2), day(2026, 11, 5)), 3);
        // Monday → Wednesday: exactly two, the alert's threshold boundary.
        assert_eq!(business_days_between(day(2026, 11, 2), day(2026, 11, 4)), 2);
        // Same instant: nothing has elapsed.
        assert_eq!(business_days_between(day(2026, 11, 2), day(2026, 11, 2)), 0);
    }

    #[test]
    fn business_days_skip_weekends() {
        // Friday → Monday counts only Monday.
        assert_eq!(business_days_between(day(2026, 11, 6), day(2026, 11, 9)), 1);
        // A full weekend in the middle of a week changes nothing.
        assert_eq!(
            business_days_between(day(2026, 11, 5), day(2026, 11, 10)),
            3
        );
        // A range that is entirely a weekend counts nothing.
        assert_eq!(business_days_between(day(2026, 11, 7), day(2026, 11, 8)), 0);
    }

    #[test]
    fn business_days_never_run_backwards_or_panic() {
        assert_eq!(business_days_between(day(2026, 11, 5), day(2026, 11, 2)), 0);
        // 2026 opens on a Thursday and closes on one: 261 weekdays in the
        // year, of which January 1st itself is outside the interval.
        assert_eq!(
            business_days_between(day(2026, 1, 1), day(2026, 12, 31)),
            260
        );
    }

    #[test]
    fn matter_lines_round_trip_through_their_stored_word() {
        assert_eq!(MatterLine::parse("plan"), Some(MatterLine::Plan));
        assert_eq!(MatterLine::parse("other"), Some(MatterLine::Other));
        assert_eq!(MatterLine::parse("pro_bono"), None);
        assert_eq!(MatterLine::Plan.as_str(), "plan");
    }
}

#[cfg(test)]
mod database {
    use super::{
        deadlines, last_activity, line_money, minutes_by_attorney_and_stage, needs_you,
        open_matters_by_line, DeadlineSource, FirmDigestError, MatterLine,
    };
    use crate::postgres::PostgresDb;
    use crate::postgres_mirror::tests::{connect, fresh_schema};
    use crate::test_support::mem_surreal;
    use chrono::{Duration, NaiveDate, TimeZone, Utc};
    use sqlx::Row;
    use uuid::Uuid;

    struct World {
        pg: PostgresDb,
        firm_id: Uuid,
        entity_id: Uuid,
        attorney: Uuid,
    }

    async fn world() -> Option<World> {
        let (options, database) = fresh_schema().await?;
        let pg = connect(&options, &database).await;
        let firm_id = Uuid::new_v4();
        let slug = format!("digest-{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO firm (id, name, slug, status) \
             VALUES ($1, 'Synthetic Digest Firm', $2, 'active')",
        )
        .bind(firm_id)
        .bind(slug)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("firm row: {error}"));
        let attorney = person(&pg, "Digest Attorney").await;
        Some(World {
            entity_id: entity(&pg).await,
            firm_id,
            pg,
            attorney,
        })
    }

    /// Jurisdiction and entity-type names are globally unique, and a test
    /// seeds a second firm, so every call mints its own reference rows.
    async fn entity(pg: &PostgresDb) -> Uuid {
        let unique = Uuid::new_v4().simple().to_string();
        let jurisdiction: Uuid = sqlx::query(
            "INSERT INTO jurisdiction (id, name, code) VALUES ($1, $2, 'DGJ') RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(format!("Synthetic Digest Jurisdiction {unique}"))
        .fetch_one(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("jurisdiction row: {error}"))
        .try_get(0)
        .unwrap_or_else(|error| panic!("jurisdiction id: {error}"));
        let entity_type: Uuid =
            sqlx::query("INSERT INTO entity_type (id, name) VALUES ($1, $2) RETURNING id")
                .bind(Uuid::new_v4())
                .bind(format!("Synthetic Digest Entity Type {unique}"))
                .fetch_one(&pg.0)
                .await
                .unwrap_or_else(|error| panic!("entity_type row: {error}"))
                .try_get(0)
                .unwrap_or_else(|error| panic!("entity_type id: {error}"));
        sqlx::query(
            "INSERT INTO entity (id, name, entity_type_id, jurisdiction_id) \
             VALUES ($1, 'Synthetic Digest Entity', $2, $3) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(entity_type)
        .bind(jurisdiction)
        .fetch_one(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("entity row: {error}"))
        .try_get(0)
        .unwrap_or_else(|error| panic!("entity id: {error}"))
    }

    async fn person(pg: &PostgresDb, name: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO person (id, name, email, role) VALUES ($1, $2, $3, 'lawyer')")
            .bind(id)
            .bind(name)
            .bind(format!(
                "{name}-{}@digest.example.com",
                Uuid::new_v4().simple()
            ))
            .execute(&pg.0)
            .await
            .unwrap_or_else(|error| panic!("person row: {error}"));
        id
    }

    async fn matter(
        pg: &PostgresDb,
        firm_id: Uuid,
        entity_id: Uuid,
        code: &str,
        plan: bool,
    ) -> Uuid {
        let legal_plan_id = if plan {
            Some(
                sqlx::query(
                    "INSERT INTO legal_plan (id, firm_id, name, acceptance_sla_hours, \
                     turnaround_sla_days) VALUES ($1, $2, 'Synthetic Plan', 24, 30) RETURNING id",
                )
                .bind(Uuid::new_v4())
                .bind(firm_id)
                .fetch_one(&pg.0)
                .await
                .unwrap_or_else(|error| panic!("legal plan: {error}"))
                .try_get::<Uuid, _>(0)
                .unwrap_or_else(|error| panic!("legal plan id: {error}")),
            )
        } else {
            None
        };
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO project (id, code, name, status, entity_id, firm_id, legal_plan_id) \
             VALUES ($1, $2, 'Synthetic Digest Matter', 'open', $3, $4, $5)",
        )
        .bind(id)
        .bind(code)
        .bind(entity_id)
        .bind(firm_id)
        .bind(legal_plan_id)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("matter row: {error}"));
        id
    }

    async fn entry(
        pg: &PostgresDb,
        project_id: Uuid,
        attorney: Uuid,
        minutes: i32,
        performed_on: NaiveDate,
    ) {
        sqlx::query(
            "INSERT INTO person_project_role (person_id, project_id, participation) \
             VALUES ($1, $2, 'lawyer') ON CONFLICT DO NOTHING",
        )
        .bind(attorney)
        .bind(project_id)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("participation row: {error}"));
        crate::time_entries::create(
            pg,
            project_id,
            attorney,
            &crate::time_entries::NewTimeEntry {
                attorney_id: attorney,
                performed_on,
                minutes,
                description: "Synthetic digest work".to_owned(),
                billable: true,
                rate_cents_per_hour: 25_000,
                source: crate::time_entries::TimeEntrySource::Manual,
            },
        )
        .await
        .unwrap_or_else(|error| panic!("time entry: {error}"));
    }

    async fn conversation(pg: &PostgresDb, firm_id: Uuid, project_id: Uuid, subject: &str) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO inbox_conversation (id, firm_id, project_id, channel, external_address, \
             external_name, subject, unread, provider_account, inserted_at, updated_at) \
             VALUES ($1, $2, $3, 'email', 'member@example.com', 'Synthetic Member', $4, true, \
             'synthetic', now(), now())",
        )
        .bind(id)
        .bind(firm_id)
        .bind(project_id)
        .bind(subject)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("conversation row: {error}"));
        id
    }

    async fn message(pg: &PostgresDb, conversation_id: Uuid, direction: &str, at: &str) {
        sqlx::query(
            "INSERT INTO message (id, conversation_id, direction, body, delivery_state, purpose, \
             occurred_at, inserted_at, updated_at) \
             VALUES ($1, $2, $3, 'Synthetic body', 'received', 'matter', $4, now(), now())",
        )
        .bind(Uuid::new_v4())
        .bind(conversation_id)
        .bind(direction)
        .bind(at)
        .execute(&pg.0)
        .await
        .unwrap_or_else(|error| panic!("message row: {error}"));
    }

    #[tokio::test]
    async fn open_matters_split_on_the_plan_columns() {
        let Some(world) = world().await else {
            return;
        };
        matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-plan",
            true,
        )
        .await;
        matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-other",
            false,
        )
        .await;
        let counts = open_matters_by_line(&world.pg, world.firm_id)
            .await
            .unwrap();
        assert_eq!(counts.get(&MatterLine::Plan), Some(&1));
        assert_eq!(counts.get(&MatterLine::Other), Some(&1));
        // A second Firm's matters never appear.
        let other_firm = Uuid::new_v4();
        let other_slug = format!("digest-other-{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO firm (id, name, slug, status) \
             VALUES ($1, 'Synthetic Other Firm', $2, 'active')",
        )
        .bind(other_firm)
        .bind(other_slug)
        .execute(&world.pg.0)
        .await
        .unwrap_or_else(|error| panic!("other firm row: {error}"));
        let counts = open_matters_by_line(&world.pg, other_firm).await.unwrap();
        assert!(counts.is_empty());
    }

    #[tokio::test]
    async fn line_money_sums_the_per_matter_pieces() {
        let Some(world) = world().await else {
            return;
        };
        let surreal = mem_surreal().await;
        let plan_matter = matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-money",
            true,
        )
        .await;
        entry(
            &world.pg,
            plan_matter,
            world.attorney,
            90,
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
        )
        .await;
        let totals = line_money(&world.pg, &surreal, world.firm_id)
            .await
            .unwrap();
        let plan = totals.get(&MatterLine::Plan).unwrap();
        // 90 minutes at 25_000 cents/hour truncates to 37_500 cents — the
        // per-entry piece the rule sums.
        assert_eq!(plan.wip_cents, 37_500);
        // No mirrored invoices and no trust notations on a fresh database.
        assert_eq!(plan.outstanding_cents, 0);
        assert_eq!(plan.trust_held_cents, 0);
    }

    #[tokio::test]
    async fn deadlines_scan_the_mirror_and_flag_missed_and_due_soon() {
        let Some(world) = world().await else {
            return;
        };
        let surreal = mem_surreal().await;
        let matter_id = matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-deadline",
            false,
        )
        .await;
        let now = Utc.with_ymd_and_hms(2026, 11, 8, 12, 0, 0).unwrap();
        for (due_at, label) in [
            ("2026-11-06T12:00:00Z", "missed"),
            ("2026-11-12T12:00:00Z", "due soon"),
            ("2027-01-01T12:00:00Z", "far off"),
        ] {
            sqlx::query(
                "INSERT INTO project_deadlines (id, project_id, due_at, authority_kind, authority, \
                 source, status, calendar_role, inserted_at, updated_at) \
                 VALUES ($1, $2, $3, 'statute', 'Synthetic statute', 'test', 'open', 'firm', now(), now())",
            )
            .bind(Uuid::new_v4())
            .bind(matter_id)
            .bind(super::parse_instant(due_at).unwrap())
            .bind(label)
            .execute(&world.pg.0)
            .await
            .unwrap_or_else(|error| panic!("deadline row: {error}"));
        }
        let scan = deadlines(&world.pg, &surreal, world.firm_id, now, 14)
            .await
            .unwrap();
        assert_eq!(scan.missed.len(), 1);
        assert_eq!(scan.due_soon.len(), 1);
        assert_eq!(scan.missed[0].label, "missed");
        assert_eq!(scan.due_soon[0].source, DeadlineSource::Deadline);
    }

    #[tokio::test]
    async fn needs_you_requires_an_unanswered_old_inbound() {
        let Some(world) = world().await else {
            return;
        };
        let now = Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap(); // Monday
        let matter_id = matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-inbox",
            false,
        )
        .await;
        let stale = conversation(&world.pg, world.firm_id, matter_id, "stale thread").await;
        // Wednesday of last week — three business days before Monday.
        message(&world.pg, stale, "inbound", "2026-11-04T12:00:00Z").await;
        let fresh = conversation(&world.pg, world.firm_id, matter_id, "fresh thread").await;
        message(&world.pg, fresh, "inbound", "2026-11-09T08:00:00Z").await;
        let answered = conversation(&world.pg, world.firm_id, matter_id, "answered thread").await;
        message(&world.pg, answered, "inbound", "2026-11-04T12:00:00Z").await;
        message(&world.pg, answered, "outbound", "2026-11-05T12:00:00Z").await;
        let items = needs_you(&world.pg, world.firm_id, now).await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].subject.as_deref(), Some("stale thread"));
    }

    #[tokio::test]
    async fn minutes_roll_up_by_attorney_line_and_stage() {
        let Some(world) = world().await else {
            return;
        };
        let plan_matter = matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-minutes",
            true,
        )
        .await;
        entry(
            &world.pg,
            plan_matter,
            world.attorney,
            60,
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
        )
        .await;
        let other_matter = matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-minutes-2",
            false,
        )
        .await;
        entry(
            &world.pg,
            other_matter,
            world.attorney,
            30,
            NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
        )
        .await;
        let rows = minutes_by_attorney_and_stage(
            &world.pg,
            world.firm_id,
            NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(),
            NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(),
        )
        .await
        .unwrap();
        let plan_row = rows
            .iter()
            .find(|row| row.line == MatterLine::Plan)
            .unwrap_or_else(|| panic!("plan row present"));
        assert_eq!(plan_row.minutes, 60);
        assert_eq!(plan_row.attorney_name.as_deref(), Some("Digest Attorney"));
        assert_eq!(plan_row.stage_label, None);
        let other_row = rows
            .iter()
            .find(|row| row.line == MatterLine::Other)
            .unwrap_or_else(|| panic!("other row present"));
        assert_eq!(other_row.minutes, 30);
    }

    #[tokio::test]
    async fn last_activity_takes_the_max_of_the_three_sources() {
        let Some(world) = world().await else {
            return;
        };
        let surreal = mem_surreal().await;
        let matter_id = matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-last",
            false,
        )
        .await;
        // A time entry Tuesday and a stage event Thursday: the stage event wins.
        entry(
            &world.pg,
            matter_id,
            world.attorney,
            45,
            NaiveDate::from_ymd_opt(2026, 11, 3).unwrap(),
        )
        .await;
        let stage_id: Uuid = sqlx::query(
            "INSERT INTO stage_definition (id, firm_id, label, position, inserted_at, updated_at) \
             VALUES ($1, $2, 'Synthetic Stage', 1, now(), now()) RETURNING id",
        )
        .bind(Uuid::new_v4())
        .bind(world.firm_id)
        .fetch_one(&world.pg.0)
        .await
        .unwrap_or_else(|error| panic!("stage row: {error}"))
        .try_get(0)
        .unwrap_or_else(|error| panic!("stage id: {error}"));
        crate::case_stages::set_stage(&world.pg, matter_id, world.attorney, stage_id, None)
            .await
            .unwrap_or_else(|error| panic!("stage event: {error}"));
        let rows = last_activity(&world.pg, &surreal, world.firm_id)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        let only = &rows[0];
        // The stage event's occurred_at is the real "now" — newer than the
        // Tuesday entry — so the max picks it over the time entry.
        let age = Utc::now()
            - only
                .last_activity
                .unwrap_or_else(|| panic!("stage event wins"));
        assert!(age < Duration::hours(1));
    }

    #[tokio::test]
    async fn a_bad_timestamp_is_a_typed_error_never_a_silent_skip() {
        let Some(world) = world().await else {
            return;
        };
        let matter_id = matter(
            &world.pg,
            world.firm_id,
            world.entity_id,
            "digest-bad",
            false,
        )
        .await;
        let bad = conversation(&world.pg, world.firm_id, matter_id, "bad timestamp").await;
        message(&world.pg, bad, "inbound", "not-a-timestamp").await;
        let now = Utc.with_ymd_and_hms(2026, 11, 9, 12, 0, 0).unwrap();
        let result = needs_you(&world.pg, world.firm_id, now).await;
        assert!(matches!(
            result,
            Err(FirmDigestError::Invalid("message.occurred_at"))
        ));
    }
}
