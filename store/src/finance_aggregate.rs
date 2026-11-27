//! The finance aggregate: a Project's fee waterfall, computed once per run and
//! kept as the latest read per Project.
//!
//! `workflows-service`'s `FinanceAggregate` workflow ingests matter rows (the
//! input document), runs [`aggregate`], and stores the result with
//! [`upsert`]; `GET /app/api/projects/{id}/finance` serves
//! [`latest_for_project`]. The report shape is the finance endpoint contract:
//! field names, whole-cent integers, and `source: "workflow"` are what the
//! Project portal's shape check requires.
//!
//! # The money model
//!
//! 1. Overhead comes off the top of collected fees.
//! 2. The rest (net) is split between the attorney and the firm.
//! 3. The firm share is split between the Lead attorney and the Rain Maker.
//!
//! Percentages are whole numbers from 0 to 100; money is whole cents.
//!
//! **Rounding rule: a share is `amount * percent / 100`, rounded half up.**
//! Overhead, the attorney share and the Lead share are rounded. Net, the firm
//! share and the Rain Maker share are what is left, so overhead + attorney
//! share + Lead share + Rain Maker share equals collected exactly for every
//! row. Aggregates **add the rounded pieces of each matter row**; a group
//! total is never split again, so every grouping sums to the same totals.
//! This is the arithmetic of the portal's own reference implementation, and
//! `tests::report_matches_the_portal_reference_cents` pins the two together
//! on a shared synthetic input.
//!
//! # Storage
//!
//! One row per run, keyed on the run id so a Restate replay of the store step
//! rewrites the same row rather than adding a second. The report is stored as
//! JSON text (the `notation_event.payload` convention): the table never needs
//! to query inside it, and opaque text survives a contract field being added.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::application_store::ApplicationStore;
use crate::postgres::accounting as authority;

/// The `source` the portal requires on a workflow-produced report.
pub const REPORT_SOURCE: &str = "workflow";

/// The three whole-number percentages a run used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Parameters {
    /// Overhead off the top of collected fees.
    pub overhead_pct: i64,
    /// Attorney share of net; the firm keeps the rest.
    pub attorney_pct: i64,
    /// Lead share of the firm share; the Rain Maker gets the rest.
    pub lead_pct: i64,
}

impl Default for Parameters {
    /// The model the portal documents: 28 / 70 / 80.
    fn default() -> Self {
        Self {
            overhead_pct: 28,
            attorney_pct: 70,
            lead_pct: 80,
        }
    }
}

/// One state in the input.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct StateRef {
    pub code: String,
    pub name: String,
}

/// One practice vertical in the input.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerticalRef {
    pub key: String,
    pub name: String,
}

/// One attorney in the input.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AttorneyRef {
    pub id: String,
    pub name: String,
    pub home_state: String,
}

/// One matter row: the unit the waterfall is computed on.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MatterRow {
    pub id: String,
    /// `YYYY-MM` the fees were collected.
    pub month: String,
    pub state: String,
    pub vertical: String,
    /// Lead attorney id.
    pub lead: String,
    /// Rain Maker attorney id; may equal `lead`.
    pub rain_maker: String,
    pub billed_cents: i64,
    pub collected_cents: i64,
}

/// The input document a run reads from object storage.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct FinanceInput {
    pub states: Vec<StateRef>,
    pub verticals: Vec<VerticalRef>,
    pub attorneys: Vec<AttorneyRef>,
    pub months: Vec<String>,
    pub matters: Vec<MatterRow>,
}

/// The money fields carried by `totals` and by every group.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Money {
    pub matters: i64,
    pub billed_cents: i64,
    pub collected_cents: i64,
    pub overhead_cents: i64,
    pub net_cents: i64,
    pub attorney_share_cents: i64,
    pub firm_share_cents: i64,
    pub lead_share_cents: i64,
    pub rain_maker_share_cents: i64,
}

impl Money {
    fn add_row(&mut self, billed: i64, split: &Split) {
        self.matters += 1;
        self.billed_cents += billed;
        self.collected_cents += split.collected;
        self.overhead_cents += split.overhead;
        self.net_cents += split.net;
        self.attorney_share_cents += split.attorney_share;
        self.firm_share_cents += split.firm_share;
        self.lead_share_cents += split.lead_share;
        self.rain_maker_share_cents += split.rain_maker_share;
    }
}

/// A named group of matters.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Group {
    pub key: String,
    pub name: String,
    #[serde(flatten)]
    pub money: Money,
}

/// An attorney group: the matters the attorney leads, plus the Rain Maker
/// credit kept apart from them.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct AttorneyGroup {
    #[serde(flatten)]
    pub group: Group,
    pub home_state: String,
    pub rain_maker_matters: i64,
    pub rain_maker_credit_cents: i64,
}

/// The finance endpoint response body.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct FinanceReport {
    pub source: String,
    pub run_id: String,
    pub computed_at: DateTime<Utc>,
    pub parameters: Parameters,
    pub totals: Money,
    pub by_month: Vec<Group>,
    pub by_state: Vec<Group>,
    pub by_vertical: Vec<Group>,
    pub by_attorney: Vec<AttorneyGroup>,
}

/// Why an input could not be aggregated.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum InputError {
    #[error("percentage {name} must be a whole number from 0 to 100")]
    Percent { name: &'static str },
    #[error("matter {id}: {field} must not be negative")]
    Negative { id: String, field: &'static str },
    #[error("matter {id}: collected exceeds billed")]
    CollectedExceedsBilled { id: String },
    #[error("matter {id}: unknown {field} `{value}`")]
    Unknown {
        id: String,
        field: &'static str,
        value: String,
    },
}

/// One matter row's waterfall, in cents.
struct Split {
    collected: i64,
    overhead: i64,
    net: i64,
    attorney_share: i64,
    firm_share: i64,
    lead_share: i64,
    rain_maker_share: i64,
}

/// `amount * percent / 100`, rounded half up. Both inputs are whole and not
/// negative (validated before any call).
fn percent_of(amount: i64, percent: i64) -> i64 {
    (amount * percent + 50) / 100
}

fn split_collected(collected: i64, parameters: Parameters) -> Split {
    let overhead = percent_of(collected, parameters.overhead_pct);
    let net = collected - overhead;
    let attorney_share = percent_of(net, parameters.attorney_pct);
    let firm_share = net - attorney_share;
    let lead_share = percent_of(firm_share, parameters.lead_pct);
    Split {
        collected,
        overhead,
        net,
        attorney_share,
        firm_share,
        lead_share,
        rain_maker_share: firm_share - lead_share,
    }
}

fn validate(input: &FinanceInput, parameters: Parameters) -> Result<(), InputError> {
    for (name, value) in [
        ("overhead_pct", parameters.overhead_pct),
        ("attorney_pct", parameters.attorney_pct),
        ("lead_pct", parameters.lead_pct),
    ] {
        if !(0..=100).contains(&value) {
            return Err(InputError::Percent { name });
        }
    }
    let known = |set: &[&str], value: &str| set.contains(&value);
    let months: Vec<&str> = input.months.iter().map(String::as_str).collect();
    let states: Vec<&str> = input.states.iter().map(|s| s.code.as_str()).collect();
    let verticals: Vec<&str> = input.verticals.iter().map(|v| v.key.as_str()).collect();
    let attorneys: Vec<&str> = input.attorneys.iter().map(|a| a.id.as_str()).collect();
    for row in &input.matters {
        for (field, value) in [
            ("billed_cents", row.billed_cents),
            ("collected_cents", row.collected_cents),
        ] {
            if value < 0 {
                return Err(InputError::Negative {
                    id: row.id.clone(),
                    field,
                });
            }
        }
        if row.collected_cents > row.billed_cents {
            return Err(InputError::CollectedExceedsBilled { id: row.id.clone() });
        }
        // A reference outside the declared groups would land in `totals` but
        // in no group, breaking "every grouping sums to the totals".
        for (field, value, ok) in [
            ("month", &row.month, known(&months, &row.month)),
            ("state", &row.state, known(&states, &row.state)),
            ("vertical", &row.vertical, known(&verticals, &row.vertical)),
            ("lead", &row.lead, known(&attorneys, &row.lead)),
            (
                "rain_maker",
                &row.rain_maker,
                known(&attorneys, &row.rain_maker),
            ),
        ] {
            if !ok {
                return Err(InputError::Unknown {
                    id: row.id.clone(),
                    field,
                    value: value.clone(),
                });
            }
        }
    }
    Ok(())
}

fn group_of(key: &str, name: &str, by_key: &BTreeMap<&str, Money>) -> Group {
    Group {
        key: key.to_string(),
        name: name.to_string(),
        money: by_key.get(key).copied().unwrap_or_default(),
    }
}

/// Compute the report for `input`. Groups appear in the input's declared
/// order, and a group with no matters appears with zeros.
///
/// # Errors
///
/// [`InputError`] when a percentage is outside 0 to 100, a cent amount is
/// negative or collected exceeds billed, or a matter names a month, state,
/// vertical or attorney the input does not declare.
pub fn aggregate(
    input: &FinanceInput,
    parameters: Parameters,
    run_id: &str,
    computed_at: DateTime<Utc>,
) -> Result<FinanceReport, InputError> {
    validate(input, parameters)?;
    let mut totals = Money::default();
    let mut months: BTreeMap<&str, Money> = BTreeMap::new();
    let mut states: BTreeMap<&str, Money> = BTreeMap::new();
    let mut verticals: BTreeMap<&str, Money> = BTreeMap::new();
    let mut led: BTreeMap<&str, Money> = BTreeMap::new();
    let mut rain_made: BTreeMap<&str, (i64, i64)> = BTreeMap::new();
    for row in &input.matters {
        let split = split_collected(row.collected_cents, parameters);
        totals.add_row(row.billed_cents, &split);
        months
            .entry(&row.month)
            .or_default()
            .add_row(row.billed_cents, &split);
        states
            .entry(&row.state)
            .or_default()
            .add_row(row.billed_cents, &split);
        verticals
            .entry(&row.vertical)
            .or_default()
            .add_row(row.billed_cents, &split);
        led.entry(&row.lead)
            .or_default()
            .add_row(row.billed_cents, &split);
        let credit = rain_made.entry(&row.rain_maker).or_default();
        credit.0 += 1;
        credit.1 += split.rain_maker_share;
    }
    Ok(FinanceReport {
        source: REPORT_SOURCE.to_string(),
        run_id: run_id.to_string(),
        computed_at,
        parameters,
        totals,
        by_month: input
            .months
            .iter()
            .map(|month| group_of(month, month, &months))
            .collect(),
        by_state: input
            .states
            .iter()
            .map(|state| group_of(&state.code, &state.name, &states))
            .collect(),
        by_vertical: input
            .verticals
            .iter()
            .map(|vertical| group_of(&vertical.key, &vertical.name, &verticals))
            .collect(),
        by_attorney: input
            .attorneys
            .iter()
            .map(|attorney| {
                let (matters, credit) = rain_made
                    .get(attorney.id.as_str())
                    .copied()
                    .unwrap_or((0, 0));
                AttorneyGroup {
                    group: group_of(&attorney.id, &attorney.name, &led),
                    home_state: attorney.home_state.clone(),
                    rain_maker_matters: matters,
                    rain_maker_credit_cents: credit,
                }
            })
            .collect(),
    })
}

/// The object-storage prefix a Project's finance run reads and writes under.
/// The input is `<prefix>/input.json`; each run's report is
/// `<prefix>/<run_id>.json`.
#[must_use]
pub fn storage_prefix(project_code: &str) -> String {
    format!("finance/{project_code}")
}

/// Object key of the input document for `project_code`.
#[must_use]
pub fn input_key(project_code: &str) -> String {
    format!("{}/input.json", storage_prefix(project_code))
}

/// Object key of one run's report for `project_code`.
#[must_use]
pub fn report_key(project_code: &str, run_id: &str) -> String {
    format!("{}/{run_id}.json", storage_prefix(project_code))
}

/// Errors reading or writing the aggregate table.
#[derive(Debug, Error)]
pub enum FinanceAggregateError {
    #[error("report document: {0}")]
    Json(#[from] serde_json::Error),
    /// PostgreSQL, under the accounting authority, failed.
    #[error(transparent)]
    Postgres(#[from] crate::postgres::accounting::AccountingAuthorityError),
}

/// The aggregate row as PostgreSQL's `to_jsonb` serves it. The report's own
/// JSON carries the shape the portal reads; only the text is needed here.
#[derive(serde::Deserialize)]
struct PgAggregateRow {
    report_json: String,
}

/// Store `report` as `project_id`'s aggregate. The record key is the run id,
/// so replaying the same run overwrites its own row.
///
/// # Errors
///
/// [`FinanceAggregateError`] when serialising or the write fails.
pub async fn upsert(
    _db: &ApplicationStore,
    project_id: Uuid,
    report: &FinanceReport,
) -> Result<(), FinanceAggregateError> {
    let report_json = serde_json::to_string(report)?;
    let pg = authority::authority().await?;
    let row = serde_json::json!({
        "id": report.run_id, "project_id": project_id, "run_id": report.run_id,
        "computed_at": report.computed_at, "parameters": report.parameters,
        "report_json": report_json,
    });
    let query = sqlx::query(
        "INSERT INTO finance_aggregate SELECT * FROM \
         jsonb_populate_record(NULL::finance_aggregate, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         ON CONFLICT (id) DO UPDATE SET project_id = EXCLUDED.project_id, \
         run_id = EXCLUDED.run_id, computed_at = EXCLUDED.computed_at, \
         parameters = EXCLUDED.parameters, report_json = EXCLUDED.report_json, \
         updated_at = now()",
    )
    .bind(row);
    authority::execute(&pg, query).await?;
    Ok(())
}

/// The newest report for `project_id`, or `None` when no run has stored one.
///
/// # Errors
///
/// [`FinanceAggregateError`] when the read or the stored document fails.
pub async fn latest_for_project(
    _db: &ApplicationStore,
    project_id: Uuid,
) -> Result<Option<FinanceReport>, FinanceAggregateError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(finance_aggregate) AS row FROM finance_aggregate \
         WHERE project_id = $1 ORDER BY computed_at DESC, run_id DESC LIMIT 1",
    )
    .bind(project_id);
    let row = authority::maybe::<PgAggregateRow>(&pg, query).await?;
    row.map(|row| serde_json::from_str(&row.report_json).map_err(Into::into))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const INPUT: &str = include_str!("../tests/data/finance/sample-finance-input.json");
    const EXPECTED: &str = include_str!("../tests/data/finance/sample-finance-expected.json");

    fn at() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap()
    }

    fn fixture() -> FinanceInput {
        serde_json::from_str(INPUT).expect("fixture parses")
    }

    #[test]
    fn rounding_is_half_up() {
        assert_eq!(percent_of(1, 50), 1);
        assert_eq!(percent_of(1, 49), 0);
        assert_eq!(percent_of(5, 10), 1);
        assert_eq!(percent_of(4, 10), 0);
        assert_eq!(percent_of(0, 28), 0);
    }

    #[test]
    fn one_row_pieces_add_back_to_collected() {
        let s = split_collected(1_000_001, Parameters::default());
        assert_eq!(
            s.overhead + s.attorney_share + s.lead_share + s.rain_maker_share,
            s.collected
        );
        assert_eq!(s.net, s.collected - s.overhead);
        assert_eq!(s.firm_share, s.net - s.attorney_share);
    }

    /// The expected file is the portal reference (`portal/src/finance.ts` and
    /// `report.ts` in the sample Project repository) run over the same input;
    /// the Rust waterfall must reproduce every cent of it. The headline
    /// totals are also written out here so a regenerated file cannot silently
    /// move them.
    #[test]
    fn report_matches_the_portal_reference_cents() {
        let report = aggregate(&fixture(), Parameters::default(), "run-x", at()).unwrap();
        assert_eq!(report.totals.matters, 633);
        assert_eq!(report.totals.billed_cents, 490_790_000);
        assert_eq!(report.totals.collected_cents, 422_331_000);
        assert_eq!(report.totals.overhead_cents, 118_252_680);
        assert_eq!(report.totals.net_cents, 304_078_320);
        assert_eq!(report.totals.attorney_share_cents, 212_854_824);
        assert_eq!(report.totals.firm_share_cents, 91_223_496);
        assert_eq!(report.totals.lead_share_cents, 72_978_802);
        assert_eq!(report.totals.rain_maker_share_cents, 18_244_694);

        let mut got = serde_json::to_value(&report).unwrap();
        let object = got.as_object_mut().unwrap();
        for key in ["source", "run_id", "computed_at"] {
            object.remove(key);
        }
        let expected: serde_json::Value = serde_json::from_str(EXPECTED).unwrap();
        assert_eq!(got, expected);
    }

    #[test]
    fn every_grouping_sums_to_the_totals() {
        let r = aggregate(&fixture(), Parameters::default(), "run-x", at()).unwrap();
        let sum = |groups: &[Group]| groups.iter().map(|g| g.money.collected_cents).sum::<i64>();
        assert_eq!(sum(&r.by_month), r.totals.collected_cents);
        assert_eq!(sum(&r.by_state), r.totals.collected_cents);
        assert_eq!(sum(&r.by_vertical), r.totals.collected_cents);
        let attorneys: i64 = r
            .by_attorney
            .iter()
            .map(|a| a.group.money.collected_cents)
            .sum();
        assert_eq!(attorneys, r.totals.collected_cents);
        let credit: i64 = r
            .by_attorney
            .iter()
            .map(|a| a.rain_maker_credit_cents)
            .sum();
        assert_eq!(credit, r.totals.rain_maker_share_cents);
        let made: i64 = r.by_attorney.iter().map(|a| a.rain_maker_matters).sum();
        assert_eq!(made, r.totals.matters);
        let t = r.totals;
        assert_eq!(
            t.overhead_cents
                + t.attorney_share_cents
                + t.lead_share_cents
                + t.rain_maker_share_cents,
            t.collected_cents
        );
    }

    #[test]
    fn report_serialises_in_the_contract_shape() {
        let r = aggregate(&fixture(), Parameters::default(), "run-x", at()).unwrap();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["source"], "workflow");
        assert_eq!(v["run_id"], "run-x");
        assert_eq!(v["computed_at"], "2026-10-01T12:00:00Z");
        assert_eq!(v["parameters"]["lead_pct"], 80);
        assert!(v["totals"]["collected_cents"].is_i64());
        let attorney = &v["by_attorney"][0];
        for field in [
            "key",
            "name",
            "home_state",
            "rain_maker_matters",
            "rain_maker_credit_cents",
            "matters",
            "rain_maker_share_cents",
        ] {
            assert!(attorney.get(field).is_some(), "missing {field}");
        }
    }

    #[test]
    fn empty_groups_still_appear_with_zeros() {
        let mut input = fixture();
        input.matters.clear();
        let r = aggregate(&input, Parameters::default(), "run-x", at()).unwrap();
        assert_eq!(r.by_month.len(), 12);
        assert_eq!(r.totals, Money::default());
        assert!(r
            .by_attorney
            .iter()
            .all(|a| a.group.money == Money::default()));
    }

    #[test]
    fn invalid_input_is_refused() {
        let mut bad = fixture();
        bad.matters[0].state = "ZZ".into();
        assert!(matches!(
            aggregate(&bad, Parameters::default(), "r", at()),
            Err(InputError::Unknown { field: "state", .. })
        ));
        let mut bad = fixture();
        bad.matters[0].collected_cents = bad.matters[0].billed_cents + 1;
        assert!(matches!(
            aggregate(&bad, Parameters::default(), "r", at()),
            Err(InputError::CollectedExceedsBilled { .. })
        ));
        let mut bad = fixture();
        bad.matters[0].billed_cents = -1;
        assert!(matches!(
            aggregate(&bad, Parameters::default(), "r", at()),
            Err(InputError::Negative { .. })
        ));
        let high = Parameters {
            lead_pct: 101,
            ..Parameters::default()
        };
        assert_eq!(
            aggregate(&fixture(), high, "r", at()),
            Err(InputError::Percent { name: "lead_pct" })
        );
        let mut bad = fixture();
        bad.matters[0].rain_maker = "nobody".into();
        assert!(aggregate(&bad, Parameters::default(), "r", at()).is_err());
    }

    #[test]
    fn storage_keys_live_under_the_project_prefix() {
        assert_eq!(storage_prefix("sample-finance"), "finance/sample-finance");
        assert_eq!(
            input_key("sample-finance"),
            "finance/sample-finance/input.json"
        );
        assert_eq!(
            report_key("sample-finance", "run-1"),
            "finance/sample-finance/run-1.json"
        );
    }
}
