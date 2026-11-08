//! `ProfitDigest`: one daily Slack post of each Firm's [Projected Contingency
//! Cash] and [Outcome Calibration], the Firm page's view, born on DBOS.
//!
//! The workflow id is `ProfitDigest-<UTC date>`, so executor replicas and
//! restarts join one run. Steps are the read of yesterday's `expected_cash`
//! event, `read` (the store rollups and the rendered text), this run's
//! `expected_cash` event, and `post`, which reuses or sends one message keyed by
//! its Effect Key, the receipt rule `GeneralNag` follows.
//!
//! Every figure comes from `store::projected_cash::firm_rollup` and
//! `store::outcome_calibration::firm_calibration`, read as the Firm's Admin
//! DRI; this module only formats them. Their labels are brand names, lead
//! lawyer names, and `Unassigned`, so the post names no matter, client, or
//! Project code. A Firm with no open contingent matter is left out, and a day
//! with none posts nothing.
//!
//! [Projected Contingency Cash]: ../../../docs/glossary/projected-contingency-cash.md
//! [Outcome Calibration]: ../../../docs/glossary/outcome-calibration.md

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, TimeDelta, Utc};
use dbos::{CancellationToken, StepOptions, WorkflowHandle, WorkflowRef, DBOS};
use serde::{Deserialize, Serialize};
use store::outcome_calibration::OutcomeCalibration;
use store::projected_cash::FirmProjectedCash;
use uuid::Uuid;

use super::daily::{self, LateStart};
use crate::SlackBot;

pub const WORKFLOW: &str = "ProfitDigest";
/// Off unless literally `true`, and only beside `NAVIGATOR_DBOS_ENABLED`.
pub const ENABLED_ENV: &str = "NAVIGATOR_DBOS_PROFIT_DIGEST_ENABLED";
/// The base URL the Firm page link is built on, `https://<public host>`.
pub const BASE_URL_ENV: &str = "NAV_BASE_URL";
/// The Firm show page's path; `webapp::firm_show::FIRM_SHOW_PATH`.
const FIRM_PAGE_PATH: &str = "/app/admin/firms";
/// The event each run publishes and the next day's run reads.
const TOTALS_EVENT: &str = "expected_cash";
/// How many quarters the post lists, starting with the current one.
pub const QUARTERS_SHOWN: i32 = 4;
const POST_ATTEMPTS: u32 = 5;

pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;
pub type ProfitDigestWorkflow = WorkflowRef<(), ProfitDigestReport, ProfitDigestError>;
/// Each Firm's expected cash in cents, keyed by Firm id.
pub type Totals = BTreeMap<Uuid, i64>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfitDigestReport {
    /// Firms the post covered; zero when nothing was posted.
    pub firms: usize,
}

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum ProfitDigestError {
    #[error("the Firm projections could not be read")]
    Read,
    #[error("the profit digest delivery could not be confirmed")]
    Notify,
}

/// One Firm's view, as the Firm page reads it.
#[derive(Debug, Clone)]
pub struct FirmProfit {
    pub firm_id: Uuid,
    pub firm_name: String,
    pub cash: FirmProjectedCash,
    pub calibration: OutcomeCalibration,
}

#[async_trait::async_trait]
pub trait ProfitSource: Send + Sync {
    /// Every Firm with an open contingent matter, on `on`.
    async fn read(&self, on: NaiveDate) -> Result<Vec<FirmProfit>, ProfitDigestError>;
}

/// The open contingent matters behind a rollup, estimated or not.
#[must_use]
pub fn contingent_matters(cash: &FirmProjectedCash) -> usize {
    cash.unestimated_matters
        + cash
            .by_brand
            .iter()
            .map(|group| group.matters)
            .sum::<usize>()
}

/// Reads each active Firm as its Admin DRI, the Owner-or-Admin viewer both
/// store rollups require.
pub struct StoreProfitSource {
    surreal: store::surreal::SurrealDb,
    pg: store::postgres::PostgresDb,
}

impl StoreProfitSource {
    #[must_use]
    pub fn new(surreal: store::surreal::SurrealDb, pg: store::postgres::PostgresDb) -> Self {
        Self { surreal, pg }
    }

    async fn admin_dri(&self, firm_id: Uuid) -> Result<Option<Uuid>, ProfitDigestError> {
        Ok(store::firms::memberships_for_firm(&self.surreal, firm_id)
            .await
            .map_err(|_| ProfitDigestError::Read)?
            .into_iter()
            .find(|role| role.is_dri && role.membership == store::firms::FirmMembership::Admin)
            .map(|role| role.person_id))
    }
}

#[async_trait::async_trait]
impl ProfitSource for StoreProfitSource {
    async fn read(&self, on: NaiveDate) -> Result<Vec<FirmProfit>, ProfitDigestError> {
        let failed = |what: &'static str| {
            move |error: &dyn std::fmt::Display| {
                tracing::error!(%error, "ProfitDigest {what} read failed");
                ProfitDigestError::Read
            }
        };
        let firms = store::firms::all(&self.surreal)
            .await
            .map_err(|error| failed("Firm")(&error))?;
        let mut out = Vec::new();
        for firm in firms.into_iter().filter(|firm| firm.status == "active") {
            let Some(viewer) = self.admin_dri(firm.id).await? else {
                tracing::warn!(firm_id = %firm.id, "ProfitDigest skips a Firm with no Admin DRI");
                continue;
            };
            let cash =
                store::projected_cash::firm_rollup(&self.surreal, &self.pg, viewer, firm.id, on)
                    .await
                    .map_err(|error| failed("projected cash")(&error))?;
            if contingent_matters(&cash) == 0 {
                continue;
            }
            let calibration = store::outcome_calibration::firm_calibration(
                &self.surreal,
                &self.pg,
                viewer,
                firm.id,
            )
            .await
            .map_err(|error| failed("calibration")(&error))?;
            out.push(FirmProfit {
                firm_id: firm.id,
                firm_name: firm.name,
                cash,
                calibration,
            });
        }
        Ok(out)
    }
}

#[derive(Clone)]
pub struct Context {
    pub source: Arc<dyn ProfitSource>,
    pub slack: Arc<dyn SlackBot>,
    pub channel_id: String,
    /// `https://<public host>`, no trailing slash.
    pub base_url: String,
}

impl Context {
    /// Resolve the live bot, this deployment's messages channel, and the base
    /// URL. A simulated-matters deployment posts to
    /// `SLACK_STAGING_MESSAGES_CHANNEL_ID`, any other to
    /// `SLACK_MESSAGES_CHANNEL_ID`.
    ///
    /// # Errors
    ///
    /// Fails when the channel, `SLACK_BOT_TOKEN`, or [`BASE_URL_ENV`] is
    /// unset or blank, or a Slack API override is set outside the CI harness.
    pub fn from_lookup<F>(
        source: Arc<dyn ProfitSource>,
        simulated: bool,
        lookup: F,
    ) -> Result<Self, super::SettingsError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let required = |key: &'static str| {
            lookup(key)
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .ok_or(super::SettingsError::Missing(key))
        };
        let channel_id = required(if simulated {
            "SLACK_STAGING_MESSAGES_CHANNEL_ID"
        } else {
            "SLACK_MESSAGES_CHANNEL_ID"
        })?;
        let token = required("SLACK_BOT_TOKEN")?;
        let base_url = required(BASE_URL_ENV)?.trim_end_matches('/').to_owned();
        let api = crate::harness_override(&lookup, "SLACK_API_BASE_URL")
            .map_err(|_| super::SettingsError::Invalid("SLACK_API_BASE_URL"))?;
        let slack: Arc<dyn SlackBot> = match api {
            Some(api) => Arc::new(crate::SlackBotClient::with_base_url(token, api)),
            None => Arc::new(crate::SlackBotClient::new(token)),
        };
        Ok(Self {
            source,
            slack,
            channel_id,
            base_url,
        })
    }
}

/// Whether this schedule is enabled. Only literal `true` opts in.
///
/// # Errors
///
/// Refuses any value other than `true` or `false`.
pub fn enabled(lookup: impl Fn(&str) -> Option<String>) -> Result<bool, super::SettingsError> {
    match lookup(ENABLED_ENV).as_deref().map(str::trim) {
        None | Some("" | "false") => Ok(false),
        Some("true") => Ok(true),
        Some(_) => Err(super::SettingsError::Invalid(ENABLED_ENV)),
    }
}

#[must_use]
pub fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

/// 14:00 UTC, the start of the Pacific morning.
#[must_use]
pub fn slot_time() -> NaiveTime {
    NaiveTime::from_hms_opt(14, 0, 0).unwrap_or(NaiveTime::MIN)
}

#[must_use]
pub fn workflow_id(slot: DateTime<Utc>) -> String {
    day_id(slot.date_naive())
}

fn day_id(day: NaiveDate) -> String {
    format!("{WORKFLOW}-{}", day.format("%Y-%m-%d"))
}

/// `123456` → `$1,234.56`; never through a float.
#[must_use]
pub fn usd(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let abs = cents.unsigned_abs();
    let digits = (abs / 100).to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    format!("{sign}${grouped}.{:02}", abs % 100)
}

/// `4250` → `42.5%`.
fn percent(bps: i64) -> String {
    format!("{}.{}%", bps / 100, (bps % 100).abs() / 10)
}

/// `1834` → `0.1834`.
fn brier(bps: i64) -> String {
    format!("{}.{:04}", bps / 10_000, bps % 10_000)
}

/// Slack's three control characters, escaped in every label.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The labels the rollup gives `on`'s quarter and the next ones.
fn upcoming_quarters(on: NaiveDate) -> impl Iterator<Item = String> {
    let first = on.year() * 4 + i32::try_from(on.month0() / 3).unwrap_or(0);
    (first..first + QUARTERS_SHOWN)
        .map(|quarter| format!("{} Q{}", quarter.div_euclid(4), quarter.rem_euclid(4) + 1))
}

fn groups(heading: &str, groups: &[store::projected_cash::CashGroup]) -> String {
    let listed: Vec<String> = groups
        .iter()
        .map(|group| {
            format!(
                "{} {} ({})",
                escape(&group.label),
                usd(group.expected_cash_cents),
                group.matters
            )
        })
        .collect();
    format!("{heading}: {}", listed.join(" · "))
}

fn change(today: i64, yesterday: Option<i64>) -> String {
    match yesterday.map(|yesterday| today - yesterday) {
        None => "no earlier digest".to_owned(),
        Some(0) => "unchanged since yesterday".to_owned(),
        Some(delta) if delta > 0 => format!("+{} since yesterday", usd(delta)),
        Some(delta) => format!("{} since yesterday", usd(delta)),
    }
}

fn render_firm(firm: &FirmProfit, yesterday: Option<i64>, base_url: &str, on: NaiveDate) -> String {
    let cash = &firm.cash;
    let mut text = format!(
        "*Projected Contingency Cash — {}* · {on}\n",
        escape(&firm.firm_name)
    );
    let (low, high) = cash.cash_range_cents;
    let _ = writeln!(
        text,
        "Expected cash: {} (range {} to {}), {}",
        usd(cash.expected_cash_cents),
        usd(low),
        usd(high),
        change(cash.expected_cash_cents, yesterday)
    );
    let _ = writeln!(text, "Costs at risk: {}", usd(cash.costs_at_risk_cents));
    let quarters: Vec<String> = upcoming_quarters(on)
        .map(|label| {
            let cents = cash
                .quarters
                .iter()
                .find(|quarter| quarter.label == label)
                .map_or(0, |quarter| quarter.projected_cash_cents);
            format!("{label} {}", usd(cents))
        })
        .collect();
    let _ = writeln!(text, "Next four quarters: {}", quarters.join(" · "));
    let _ = writeln!(text, "{}", groups("By brand", &cash.by_brand));
    let _ = writeln!(text, "{}", groups("By lead lawyer", &cash.by_lead_lawyer));
    if let Some(share) = cash.concentration_bps {
        let _ = writeln!(
            text,
            ":warning: One matter holds {} of the expected cash.",
            percent(share)
        );
    }
    if cash.unestimated_matters > 0 {
        let _ = writeln!(
            text,
            "Open contingent matters with no recovery estimated yet: {}.",
            cash.unestimated_matters
        );
    }
    let calibration = &firm.calibration;
    let _ = match &calibration.report {
        Some(report) => writeln!(
            text,
            "Outcome Calibration: Brier score {} over {} resolved matters.",
            brier(report.brier_bps),
            calibration.scored_matters
        ),
        None => writeln!(
            text,
            "Outcome Calibration needs at least {} resolved matters with an estimate in force; {} so far.",
            calibration.minimum_matters, calibration.scored_matters
        ),
    };
    let _ = write!(
        text,
        "<{base_url}{FIRM_PAGE_PATH}/{}|Open the Firm page>",
        firm.firm_id
    );
    text
}

/// The post for `firms`, or `None` when no Firm has an open contingent matter.
#[must_use]
pub fn render(
    firms: &[FirmProfit],
    yesterday: Option<&Totals>,
    base_url: &str,
    on: NaiveDate,
) -> Option<String> {
    let sections: Vec<String> = firms
        .iter()
        .filter(|firm| contingent_matters(&firm.cash) > 0)
        .map(|firm| {
            let before = yesterday.and_then(|totals| totals.get(&firm.firm_id).copied());
            render_firm(firm, before, base_url, on)
        })
        .collect();
    (!sections.is_empty()).then(|| sections.join("\n\n"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Digest {
    totals: Totals,
    text: Option<String>,
}

/// The run's UTC date, from its workflow id.
fn run_day() -> Option<NaiveDate> {
    dbos::workflow_id()?
        .strip_prefix(&format!("{WORKFLOW}-"))
        .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
}

pub fn register(dbos: &DBOS, context: Context) -> dbos::Result<ProfitDigestWorkflow> {
    dbos.register_workflow(WORKFLOW, move |()| {
        let context = context.clone();
        async move {
            let on = run_day().ok_or(dbos::Error::Application(ProfitDigestError::Read))?;
            let yesterday_id = day_id(on - TimeDelta::days(1));
            let yesterday: Option<Totals> =
                dbos::get_event(&yesterday_id, TOTALS_EVENT, Duration::ZERO).await?;
            let (source, base_url) = (Arc::clone(&context.source), context.base_url.clone());
            let digest = dbos::step("read", move || {
                let (source, base_url, yesterday) =
                    (Arc::clone(&source), base_url.clone(), yesterday.clone());
                async move {
                    let firms = source.read(on).await.map_err(dbos::Error::Application)?;
                    Ok::<_, dbos::Error<ProfitDigestError>>(Digest {
                        totals: firms
                            .iter()
                            .map(|firm| (firm.firm_id, firm.cash.expected_cash_cents))
                            .collect(),
                        text: render(&firms, yesterday.as_ref(), &base_url, on),
                    })
                }
            })
            .await?;
            dbos::set_event::<_, ProfitDigestError>(TOTALS_EVENT, &digest.totals).await?;
            let Some(text) = digest.text else {
                return Ok(ProfitDigestReport { firms: 0 });
            };
            let oldest = on.and_time(NaiveTime::MIN).and_utc();
            let (slack, channel_id) = (Arc::clone(&context.slack), context.channel_id.clone());
            dbos::step_with(
                "post",
                StepOptions {
                    max_attempts: POST_ATTEMPTS,
                    ..StepOptions::default()
                },
                move || {
                    let (slack, channel_id, text) =
                        (Arc::clone(&slack), channel_id.clone(), text.clone());
                    async move {
                        if super::post_once(slack.as_ref(), &channel_id, &text, oldest).await {
                            Ok(())
                        } else {
                            Err(dbos::Error::Application(ProfitDigestError::Notify))
                        }
                    }
                },
            )
            .await?;
            dbos::Result::<_, ProfitDigestError>::Ok(ProfitDigestReport {
                firms: digest.totals.len(),
            })
        }
    })
}

pub async fn fire(
    workflow: &ProfitDigestWorkflow,
    slot: DateTime<Utc>,
) -> dbos::Result<WorkflowHandle<ProfitDigestReport, ProfitDigestError>, ProfitDigestError> {
    let id = workflow_id(slot);
    workflow
        .start_with(
            (),
            dbos::StartOptions {
                workflow_id: Some(&id),
                ..dbos::StartOptions::default()
            },
        )
        .await
}

/// A late executor still posts today's digest; a restart joins it.
pub async fn schedule(workflow: ProfitDigestWorkflow, clock: Clock, stop: CancellationToken) {
    daily::run(
        WORKFLOW,
        slot_time(),
        LateStart::CatchUp,
        move || clock(),
        stop,
        |slot| {
            let workflow = &workflow;
            async move { fire(workflow, slot).await.map(|_| ()) }
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::{
        contingent_matters, enabled, render, usd, workflow_id, Context, FirmProfit, ProfitSource,
        Totals, ENABLED_ENV,
    };
    use crate::dbos_engine::SettingsError;
    use chrono::{NaiveDate, TimeZone, Utc};
    use std::sync::Arc;
    use store::outcome_calibration::{CalibrationReport, OutcomeCalibration};
    use store::projected_cash::{CashGroup, FirmProjectedCash, FirmQuarter};
    use uuid::Uuid;

    const BASE: &str = "https://navigator.example.invalid";

    fn on() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 7).unwrap()
    }

    fn group(label: &str, cents: i64, matters: usize) -> CashGroup {
        CashGroup {
            label: label.to_owned(),
            expected_cash_cents: cents,
            matters,
        }
    }

    fn quarter(label: &str, cents: i64) -> FirmQuarter {
        FirmQuarter {
            label: label.to_owned(),
            projected_cash_cents: cents,
            invoiced_cents: 0,
            paid_cents: 0,
        }
    }

    fn calibration(scored: usize, report: Option<CalibrationReport>) -> OutcomeCalibration {
        OutcomeCalibration {
            lookback_days: 30,
            minimum_matters: 10,
            scored_matters: scored,
            unscored_matters: 0,
            report,
        }
    }

    fn firm(firm_id: Uuid) -> FirmProfit {
        FirmProfit {
            firm_id,
            firm_name: "Neon Law".to_owned(),
            cash: FirmProjectedCash {
                expected_cash_cents: 12_594_000,
                cash_range_cents: (0, 34_400_000),
                quarters: vec![
                    quarter("2026 Q3", 0),
                    quarter("2026 Q4", 6_297_000),
                    quarter("2027 Q1", 6_297_000),
                    quarter("Later", 99),
                ],
                by_brand: vec![group("Neon Law", 12_594_000, 2)],
                by_lead_lawyer: vec![
                    group("Lawyer One", 6_297_000, 1),
                    group("Unassigned", 6_297_000, 1),
                ],
                concentration_bps: Some(5_000),
                unestimated_matters: 1,
                costs_at_risk_cents: 3_600_000,
            },
            calibration: calibration(3, None),
        }
    }

    #[test]
    fn money_is_grouped_dollars_from_integer_cents() {
        assert_eq!(usd(0), "$0.00");
        assert_eq!(usd(5), "$0.05");
        assert_eq!(usd(123_456), "$1,234.56");
        assert_eq!(usd(-100_000_000), "-$1,000,000.00");
    }

    #[test]
    fn the_post_shows_the_firm_view_and_links_the_firm_page() {
        let id = Uuid::new_v4();
        let text = render(&[firm(id)], None, BASE, on()).unwrap();
        for expected in [
            "*Projected Contingency Cash — Neon Law* · 2026-11-07",
            "Expected cash: $125,940.00 (range $0.00 to $344,000.00), no earlier digest",
            "Costs at risk: $36,000.00",
            "Next four quarters: 2026 Q4 $62,970.00 · 2027 Q1 $62,970.00 · 2027 Q2 $0.00 · 2027 Q3 $0.00",
            "By brand: Neon Law $125,940.00 (2)",
            "By lead lawyer: Lawyer One $62,970.00 (1) · Unassigned $62,970.00 (1)",
            ":warning: One matter holds 50.0% of the expected cash.",
            "no recovery estimated yet: 1.",
            "needs at least 10 resolved matters with an estimate in force; 3 so far.",
        ] {
            assert!(text.contains(expected), "{expected} missing from {text}");
        }
        assert!(text.ends_with(&format!("<{BASE}/app/admin/firms/{id}|Open the Firm page>")));
        assert!(!text.contains("Brier"), "empty state only: {text}");
    }

    #[test]
    fn a_scored_firm_shows_its_brier_score_and_the_change_since_yesterday() {
        let id = Uuid::new_v4();
        let mut scored = firm(id);
        scored.calibration = calibration(
            12,
            Some(CalibrationReport {
                brier_bps: 1_834,
                mean_recovery_error_cents: 0,
                mean_absolute_recovery_error_cents: 0,
                bands: vec![],
                by_lead_lawyer: vec![],
                by_case_stage: vec![],
            }),
        );
        let rise: Totals = [(id, 12_000_000)].into();
        let text = render(&[scored.clone()], Some(&rise), BASE, on()).unwrap();
        assert!(text.contains("+$5,940.00 since yesterday"), "{text}");
        assert!(
            text.contains("Brier score 0.1834 over 12 resolved matters."),
            "{text}"
        );
        let fall: Totals = [(id, 13_000_000)].into();
        let text = render(&[scored.clone()], Some(&fall), BASE, on()).unwrap();
        assert!(text.contains("-$4,060.00 since yesterday"), "{text}");
        let same: Totals = [(id, 12_594_000)].into();
        let text = render(&[scored], Some(&same), BASE, on()).unwrap();
        assert!(text.contains("unchanged since yesterday"), "{text}");
    }

    #[test]
    fn a_firm_with_no_contingent_matter_posts_nothing() {
        let mut empty = firm(Uuid::new_v4());
        empty.cash = FirmProjectedCash::default();
        assert_eq!(contingent_matters(&empty.cash), 0);
        assert_eq!(render(&[empty], None, BASE, on()), None);
        assert_eq!(render(&[], None, BASE, on()), None);
    }

    #[test]
    fn no_seeded_matter_client_or_project_code_reaches_the_post() {
        let mut leaky = firm(Uuid::new_v4());
        leaky.cash.by_lead_lawyer.push(group("A & <B>", 1, 1));
        let text = render(&[leaky], None, BASE, on()).unwrap();
        assert!(
            text.contains("A &amp; &lt;B&gt;"),
            "labels are escaped: {text}"
        );
        for forbidden in store::seed::sample_matter_names()
            .into_iter()
            .chain(store::seed::sample_matter_clients())
            .chain(store::seed::sample_matter_codes())
        {
            assert!(!text.contains(forbidden), "{forbidden} leaked into {text}");
        }
    }

    #[test]
    fn the_schedule_is_off_unless_true_and_keyed_by_utc_date() {
        assert!(!enabled(|_| None).unwrap());
        assert!(!enabled(|_| Some("false".into())).unwrap());
        assert!(enabled(|_| Some("true".into())).unwrap());
        assert_eq!(
            enabled(|_| Some("yes".into())).unwrap_err(),
            SettingsError::Invalid(ENABLED_ENV)
        );
        let late = Utc.with_ymd_and_hms(2026, 11, 7, 23, 59, 0).unwrap();
        assert_eq!(workflow_id(late), "ProfitDigest-2026-11-07");
    }

    struct NoFirms;

    #[async_trait::async_trait]
    impl ProfitSource for NoFirms {
        async fn read(&self, _on: NaiveDate) -> Result<Vec<FirmProfit>, super::ProfitDigestError> {
            Ok(vec![])
        }
    }

    #[test]
    fn each_environment_resolves_its_own_channel_and_refuses_a_blank_one() {
        const BOTH: &[(&str, &str)] = &[
            ("SLACK_MESSAGES_CHANNEL_ID", "C-MESSAGES"),
            ("SLACK_STAGING_MESSAGES_CHANNEL_ID", "C-STAGING"),
            ("SLACK_BOT_TOKEN", "xoxb-test"),
            ("NAV_BASE_URL", "https://navigator.example.invalid/"),
        ];
        let configured = |simulated: bool, pairs: &'static [(&'static str, &'static str)]| {
            Context::from_lookup(Arc::new(NoFirms), simulated, move |key| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| (*value).to_owned())
            })
        };
        assert_eq!(configured(false, BOTH).unwrap().channel_id, "C-MESSAGES");
        let staging = configured(true, BOTH).unwrap();
        assert_eq!(staging.channel_id, "C-STAGING");
        assert_eq!(staging.base_url, BASE);
        assert!(matches!(
            configured(false, &[("SLACK_MESSAGES_CHANNEL_ID", "  ")]),
            Err(SettingsError::Missing("SLACK_MESSAGES_CHANNEL_ID"))
        ));
        assert!(matches!(
            configured(true, &[("SLACK_MESSAGES_CHANNEL_ID", "C-MESSAGES")]),
            Err(SettingsError::Missing("SLACK_STAGING_MESSAGES_CHANNEL_ID"))
        ));
        assert!(matches!(
            configured(
                false,
                &[
                    ("SLACK_MESSAGES_CHANNEL_ID", "C-MESSAGES"),
                    ("SLACK_BOT_TOKEN", "xoxb-test")
                ]
            ),
            Err(SettingsError::Missing("NAV_BASE_URL"))
        ));
    }
}
