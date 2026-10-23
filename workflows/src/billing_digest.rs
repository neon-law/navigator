//! Provider-independent GCP billing digest model, query, and renderer.

use crate::OutboundEmail;
use billing::gcp_cost::{
    adc_token_provider, billing_account_from_table, billing_export_tables, BillingClient, CostRow,
    ProjectCost,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Default digest recipient when `BILLING_DIGEST_NOTIFY_EMAIL` is unset.
const DEFAULT_NOTIFY_EMAIL: &str = "nick@neonlaw.com";

/// Default trailing window in days when `BILLING_DIGEST_WINDOW_DAYS` is unset.
const DEFAULT_WINDOW_DAYS: u32 = 30;

/// One billing account's slice of the window — the same four groupings the
/// digest renders, scoped to the single export table that account writes.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct AccountCost {
    /// Billing account id (`013469-2BBE03-532C72`), derived from the export
    /// table name by [`billing::gcp_cost::billing_account_from_table`].
    pub account: String,
    /// Current-window cost by service within this account, highest first.
    pub current: Vec<CostRow>,
    /// Prior-window (days `window..2*window`) cost by service, for the trend.
    pub prior: Vec<CostRow>,
    /// Current-window cost by project within this account, highest first.
    pub current_by_project: Vec<ProjectCost>,
    /// Prior-window cost by project, for the per-project trend.
    pub prior_by_project: Vec<ProjectCost>,
}

impl AccountCost {
    #[must_use]
    pub fn cost_total(&self) -> f64 {
        visible_cost_total(self.current.iter().map(|c| c.cost))
    }

    fn prior_cost_total(&self) -> f64 {
        visible_cost_total(self.prior.iter().map(|c| c.cost))
    }
}

/// The journaled result of the query step, carried into the email render. A
/// pure value (no clients, no env) so the renderer is unit-testable.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct BillingDigestReport {
    /// Trailing window the costs cover, in days.
    pub window_days: u32,
    /// Instant the query step ran (journaled), so the rendered date is stable
    /// on replay rather than re-reading the clock.
    pub as_of: DateTime<Utc>,
    /// One entry per configured billing account, in `BILLING_EXPORT_TABLE`
    /// order.
    ///
    /// `#[serde(default)]` because this value is journaled: an invocation whose
    /// query step was recorded by a release that grouped only by service must
    /// still deserialize. It replays as "no accounts" and renders the no-rows
    /// note — a degraded message for the one invocation that spans a deploy,
    /// rather than a permanently failing one.
    #[serde(default)]
    pub accounts: Vec<AccountCost>,
}

impl BillingDigestReport {
    /// Spend across every account in the window — the headline figure.
    pub fn cost_total(&self) -> f64 {
        self.accounts.iter().map(AccountCost::cost_total).sum()
    }

    /// Distinct services with visible spend, counted across all accounts — one
    /// service billed to two accounts is one service.
    #[must_use]
    pub fn service_count(&self) -> usize {
        self.accounts
            .iter()
            .flat_map(|a| a.current.iter())
            .filter(|c| is_visible_cost(c.cost))
            .map(|c| c.service.as_str())
            .collect::<std::collections::HashSet<_>>()
            .len()
    }

    /// Projects with visible spend, across all accounts. A project belongs to
    /// exactly one billing account, so these cannot double-count.
    #[must_use]
    pub fn project_count(&self) -> usize {
        self.accounts
            .iter()
            .flat_map(|a| a.current_by_project.iter())
            .filter(|p| is_visible_cost(p.cost))
            .count()
    }

    /// Every account the digest actually read, for the covered-accounts line.
    fn account_ids(&self) -> Vec<&str> {
        self.accounts.iter().map(|a| a.account.as_str()).collect()
    }
}

/// Invocation output: whether a digest was sent, and the headline cost figure
/// when it was. `sent == false` is the unconfigured no-op (no export).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct DigestOutcome {
    pub sent: bool,
    pub cost_total: f64,
    pub services: usize,
    /// Billing-account projects with visible spend in the window.
    #[serde(default)]
    pub projects: usize,
}

/// Resolved configuration for one digest run.
#[derive(Debug, Clone, PartialEq)]
pub struct DigestConfig {
    pub recipient: String,
    pub window_days: u32,
    /// `Some` only when both the export table and project are configured;
    /// `None` is the unconfigured no-op (no send).
    pub query: Option<QueryConfig>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueryConfig {
    /// One export table per billing account, in configured order. Never empty —
    /// an empty list is the unconfigured no-op and yields `None` instead.
    pub tables: Vec<String>,
    /// The project whose BigQuery quota runs the queries. One project can query
    /// every account's export it has `dataViewer` on, so this stays singular.
    pub project: String,
}

impl DigestConfig {
    /// Resolve from a `key -> value` lookup (`std::env::var` in production) so
    /// the gating is unit-testable without mutating process env.
    pub fn from_env<F: Fn(&str) -> Option<String>>(get: F) -> Self {
        let non_empty = |k: &str| get(k).filter(|s| !s.is_empty());
        let recipient = non_empty("BILLING_DIGEST_NOTIFY_EMAIL")
            .unwrap_or_else(|| DEFAULT_NOTIFY_EMAIL.to_string());
        let window_days = non_empty("BILLING_DIGEST_WINDOW_DAYS")
            .and_then(|s| s.parse().ok())
            .filter(|d| *d > 0)
            .unwrap_or(DEFAULT_WINDOW_DAYS);
        // At least one table AND a project, or we can't query — treat either
        // unset as the unconfigured no-op so a half-configured fork doesn't
        // crash-loop. `BILLING_EXPORT_TABLE` is a comma-separated list, one
        // table per billing account.
        let query = match (
            non_empty("BILLING_EXPORT_TABLE").map(|raw| billing_export_tables(&raw)),
            non_empty("BIGQUERY_PROJECT"),
        ) {
            (Some(tables), Some(project)) if !tables.is_empty() => {
                Some(QueryConfig { tables, project })
            }
            _ => None,
        };
        Self {
            recipient,
            window_days,
            query,
        }
    }
}

#[must_use]
pub fn dollars(v: f64) -> String {
    let cents = rounded_cents(v);
    let magnitude = cents.unsigned_abs();
    let sign = if cents < 0 { "-" } else { "" };
    format!("{sign}${}.{:02}", magnitude / 100, magnitude % 100)
}

// The GCP export provides finite, ordinary billing amounts. `as` has defined
// saturating semantics for values outside i64, and rounding before conversion
// preserves the digest's cents-based visibility and display policy.
#[allow(clippy::cast_possible_truncation)]
fn rounded_cents(v: f64) -> i64 {
    (v * 100.0).round() as i64
}

fn is_visible_cost(v: f64) -> bool {
    rounded_cents(v) != 0
}

fn visible_cost_total(costs: impl Iterator<Item = f64>) -> f64 {
    costs.filter(|c| is_visible_cost(*c)).sum()
}

/// Query each configured account and return the journalable report.
pub async fn query_report(query: &QueryConfig, window: u32) -> Result<BillingDigestReport, String> {
    let token = adc_token_provider()
        .await
        .map_err(|error| error.to_string())?;
    let client = BillingClient::new(query.project.clone(), token);
    let mut accounts = Vec::with_capacity(query.tables.len());
    for table in &query.tables {
        accounts.push(AccountCost {
            account: billing_account_from_table(table),
            current: client
                .cost_by_service_window(table, window, 0)
                .await
                .map_err(|error| error.to_string())?,
            prior: client
                .cost_by_service_window(table, window * 2, window)
                .await
                .map_err(|error| error.to_string())?,
            current_by_project: client
                .cost_by_project_window(table, window, 0)
                .await
                .map_err(|error| error.to_string())?,
            prior_by_project: client
                .cost_by_project_window(table, window * 2, window)
                .await
                .map_err(|error| error.to_string())?,
        });
    }
    Ok(BillingDigestReport {
        window_days: window,
        as_of: Utc::now(),
        accounts,
    })
}

/// Build the daily billing-digest email. Pure — exposed so the rendered
/// subject/body is unit-tested without a worker or BigQuery.
#[must_use]
pub fn build_digest_email(report: &BillingDigestReport, recipient: &str) -> OutboundEmail {
    use std::fmt::Write as _;

    let date = report.as_of.format("%Y-%m-%d");
    let total = report.cost_total();
    // Name the account count in the subject once there is more than one, so a
    // silently-dropped account is visible without opening the message.
    let across = match report.accounts.len() {
        0 | 1 => String::new(),
        n => format!(" across {n} accounts"),
    };
    let subject = format!(
        "💸 {} {}-day GCP cost{across} — {date}",
        dollars(total),
        report.window_days
    );

    let mut out = String::with_capacity(2048);
    let _ = writeln!(
        out,
        "{} {}-day GCP cost as of {date} UTC.",
        dollars(total),
        report.window_days
    );
    let _ = writeln!(
        out,
        "Every project on every configured GCP billing account, by account, project, and service; \
         the current day may be partial because the billing export lags by roughly 24 hours.\n"
    );

    // Name the accounts read, always. This is what makes "all accounts"
    // checkable from the message itself: an account missing from the total is
    // missing from this line too, instead of being invisible.
    let account_ids = report.account_ids();
    let _ = writeln!(
        out,
        "BILLING ACCOUNTS COVERED ({}): {}\n",
        account_ids.len(),
        if account_ids.is_empty() {
            "none".to_string()
        } else {
            account_ids.join(", ")
        }
    );

    if report.accounts.iter().all(|a| a.current.is_empty()) {
        // Configured but no rows in the window — say so plainly rather than
        // render a misleading all-zero table (the export may be lagging ~24h).
        out.push_str(
            "No billing rows in the trailing window. The billing export lags ~24h, so this can \
             mean the most recent days haven't landed yet — check the BigQuery export freshness \
             if it persists.\n",
        );
    } else {
        // With more than one account, lead with the cross-account roll-up so
        // the headline total is decomposed before any account's detail. With
        // one, that table would just restate the total.
        if report.accounts.len() > 1 {
            write_cost_table(
                &mut out,
                &format!("GCP COST BY BILLING ACCOUNT ({} DAYS)", report.window_days),
                "Billing account",
                report
                    .accounts
                    .iter()
                    .map(|a| (a.account.as_str(), a.cost_total())),
                report
                    .accounts
                    .iter()
                    .map(|a| (a.account.as_str(), a.prior_cost_total())),
            );
        }

        let window = report.window_days;
        for account in &report.accounts {
            // Head each account's detail only when there is more than one, so
            // the single-account message keeps its two clean tables.
            if report.accounts.len() > 1 {
                let _ = writeln!(
                    out,
                    "ACCOUNT {} — {}\n",
                    account.account,
                    dollars(account.cost_total())
                );
            }
            // By project first: naming the project is what makes the money
            // attributable; the service split explains what it was spent on.
            write_cost_table(
                &mut out,
                &format!("GCP COST BY PROJECT ({window} DAYS)"),
                "Project",
                account
                    .current_by_project
                    .iter()
                    .map(|p| (p.project.as_str(), p.cost)),
                account
                    .prior_by_project
                    .iter()
                    .map(|p| (p.project.as_str(), p.cost)),
            );
            write_cost_table(
                &mut out,
                &format!("GCP COST BY SERVICE ({window} DAYS)"),
                "Service",
                account.current.iter().map(|c| (c.service.as_str(), c.cost)),
                account.prior.iter().map(|c| (c.service.as_str(), c.cost)),
            );
        }
    }

    let html = crate::email::render_email_html(&out, &crate::email::base_url_from_env());
    OutboundEmail::new(recipient.to_string(), subject, out).with_html(html)
}

/// Render one fixed-width `label / cost / vs prior` block with a TOTAL row,
/// summing what the block actually displays. BigQuery rounds each group's
/// `SUM(cost)` to cents independently, so the by-project and by-service TOTALs
/// can differ by a cent or two over the same window; each is honest about the
/// rows above it, which matters more than forcing them to agree.
///
/// Rows that round to $0.00 are skipped. Shared by the by-project and
/// by-service sections so both read identically and neither can drift in shape.
/// A row absent from the prior window shows `n/a` rather than a fake delta; a
/// block whose every row rounds to zero is omitted entirely.
fn write_cost_table<'a>(
    out: &mut String,
    heading: &str,
    label_header: &str,
    current: impl Iterator<Item = (&'a str, f64)>,
    prior: impl Iterator<Item = (&'a str, f64)>,
) {
    use std::fmt::Write as _;

    let rows: Vec<(&str, f64)> = current.filter(|(_, cost)| is_visible_cost(*cost)).collect();
    if rows.is_empty() {
        return;
    }
    let prior_by_label: std::collections::HashMap<&str, f64> = prior.collect();

    let _ = writeln!(out, "{heading}\n");
    let _ = writeln!(
        out,
        "{label_header:<30}  {:>12}  {:>12}",
        "Cost", "vs prior"
    );
    let _ = writeln!(out, "{:-<30}  {:-<12}  {:-<12}", "", "", "");
    let mut total = 0.0;
    for (label, cost) in &rows {
        total += cost;
        let delta = prior_by_label.get(label).map(|prev| cost - prev);
        let _ = writeln!(
            out,
            "{:<30}  {:>12}  {:>12}",
            truncate(label, 30),
            dollars(*cost),
            delta.map_or_else(|| "n/a".to_string(), signed_dollars),
        );
    }
    let prior_total = visible_cost_total(prior_by_label.values().copied());
    let _ = writeln!(out, "{:-<30}  {:-<12}  {:-<12}", "", "", "");
    let _ = writeln!(
        out,
        "{:<30}  {:>12}  {:>12}",
        "TOTAL",
        dollars(total),
        signed_dollars(total - prior_total),
    );
    out.push('\n');
}

/// Truncate a service name to `max` chars for the fixed-width table.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

#[must_use]
pub fn signed_dollars(v: f64) -> String {
    match rounded_cents(v).cmp(&0) {
        std::cmp::Ordering::Greater => format!("+{}", dollars(v)),
        std::cmp::Ordering::Less => dollars(v),
        std::cmp::Ordering::Equal => "$0.00".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_digest_email, dollars, signed_dollars, AccountCost, BillingDigestReport, DigestConfig,
    };
    use billing::gcp_cost::{CostRow, ProjectCost};
    use chrono::{DateTime, Utc};

    /// The firm's real billing account — the one with the configured export.
    const FIRM_ACCOUNT: &str = "013469-2BBE03-532C72";
    /// A second account, to prove the digest is not structurally single-account.
    const SECOND_ACCOUNT: &str = "01528F-251BB0-54E54F";

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn project(project: &str, cost: f64) -> ProjectCost {
        ProjectCost {
            project: project.into(),
            cost,
        }
    }

    fn service(service: &str, cost: f64) -> CostRow {
        CostRow {
            service: service.into(),
            cost,
        }
    }

    /// One account carrying $133.76, grouped both ways.
    fn firm_account() -> AccountCost {
        AccountCost {
            account: FIRM_ACCOUNT.into(),
            // The same $133.76 the service rows below carry, split across the
            // billing account's projects.
            current_by_project: vec![
                project("neon-law-420305", 120.00),
                project("neon-law-test", 13.76),
                project("ghcr", 0.004),
            ],
            prior_by_project: vec![project("neon-law-420305", 100.00)],
            current: vec![
                CostRow {
                    service: "Kubernetes Engine".into(),
                    cost: 114.44,
                },
                CostRow {
                    service: "Cloud SQL".into(),
                    cost: 19.32,
                },
                CostRow {
                    service: "Artifact Registry".into(),
                    cost: 0.004,
                },
                CostRow {
                    service: "Cloud Trace".into(),
                    cost: 0.004,
                },
                CostRow {
                    service: "Invoice".into(),
                    cost: 0.004,
                },
            ],
            prior: vec![CostRow {
                service: "Kubernetes Engine".into(),
                cost: 100.00,
            }],
        }
    }

    /// The single-account report — the shape a deploy with one
    /// `BILLING_EXPORT_TABLE` produces.
    fn sample_report() -> BillingDigestReport {
        BillingDigestReport {
            window_days: 30,
            as_of: ts("2026-06-15T13:00:00Z"),
            accounts: vec![firm_account()],
        }
    }

    /// Two accounts, the shape a deploy with a comma-separated
    /// `BILLING_EXPORT_TABLE` produces: $133.76 + $50.00.
    fn two_account_report() -> BillingDigestReport {
        BillingDigestReport {
            window_days: 30,
            as_of: ts("2026-06-15T13:00:00Z"),
            accounts: vec![
                firm_account(),
                AccountCost {
                    account: SECOND_ACCOUNT.into(),
                    current: vec![service("Compute Engine", 50.00)],
                    prior: vec![service("Compute Engine", 40.00)],
                    current_by_project: vec![project("side-project", 50.00)],
                    prior_by_project: vec![project("side-project", 40.00)],
                },
            ],
        }
    }

    #[test]
    fn digest_reports_every_billing_account_project_before_the_service_table() {
        let report = sample_report();
        // Two projects clear the rounding floor; ghcr's $0.004 does
        // not, exactly as a $0.004 service row does not.
        assert_eq!(report.project_count(), 2);

        let b = build_digest_email(&report, "ops@example.com").body;
        assert!(
            b.contains("Every project on every configured GCP billing account"),
            "scope line missing: {b}"
        );
        // The account is named even when there is only one, so "all accounts"
        // is checkable from the message rather than assumed.
        assert!(
            b.contains(&format!("BILLING ACCOUNTS COVERED (1): {FIRM_ACCOUNT}")),
            "covered-accounts line missing: {b}"
        );
        assert!(b.contains("GCP COST BY PROJECT (30 DAYS)"), "{b}");
        assert!(b.contains("neon-law-420305"), "{b}");
        assert!(b.contains("neon-law-test"), "{b}");
        assert!(!b.contains("ghcr"), "$0.00 project shown: {b}");
        // Per-project trend: present in the prior window → a delta; absent → n/a.
        assert!(b.contains("+$20.00"), "project delta: {b}");

        // The by-project total is the same all-project money the by-service
        // total reports — they are two groupings of one window.
        let project_table = b
            .split("GCP COST BY SERVICE")
            .next()
            .expect("project table precedes the service table");
        assert!(project_table.contains("TOTAL"), "{project_table}");
        assert!(project_table.contains("$133.76"), "{project_table}");
        assert!(project_table.contains("+$33.76"), "{project_table}");

        // The project table comes first — the headline is which project spent.
        assert!(
            b.find("BY PROJECT") < b.find("BY SERVICE"),
            "project table must precede the service table: {b}"
        );
    }

    #[test]
    fn digest_totals_across_every_configured_billing_account() {
        let report = two_account_report();
        // The headline is the sum over accounts, not one account's slice.
        assert_eq!(dollars(report.cost_total()), "$183.76");
        assert_eq!(report.project_count(), 3);
        // Kubernetes Engine, Cloud SQL, Compute Engine — distinct across
        // accounts, so a service billed to two accounts counts once.
        assert_eq!(report.service_count(), 3);

        let email = build_digest_email(&report, "ops@example.com");
        // The count is in the subject, so a dropped account shows in the
        // notification list without opening the message.
        assert!(
            email
                .subject
                .starts_with("💸 $183.76 30-day GCP cost across 2 accounts"),
            "subject: {}",
            email.subject
        );

        let b = &email.body;
        assert!(
            b.contains(&format!(
                "BILLING ACCOUNTS COVERED (2): {FIRM_ACCOUNT}, {SECOND_ACCOUNT}"
            )),
            "covered-accounts line: {b}"
        );

        // A cross-account roll-up leads, decomposing the headline before any
        // single account's detail.
        // Split on the per-account header at line start — "ACCOUNT " alone
        // also matches the "BY BILLING ACCOUNT" heading inside the roll-up.
        let rollup = b
            .split("\nACCOUNT ")
            .next()
            .expect("roll-up precedes the per-account sections");
        assert!(
            rollup.contains("GCP COST BY BILLING ACCOUNT (30 DAYS)"),
            "{rollup}"
        );
        assert!(rollup.contains(FIRM_ACCOUNT) && rollup.contains(SECOND_ACCOUNT));
        assert!(
            rollup.contains("$133.76") && rollup.contains("$50.00"),
            "{rollup}"
        );
        assert!(rollup.contains("$183.76"), "grand total: {rollup}");
        // Prior across both accounts is $100.00 + $40.00 = $140.00.
        assert!(rollup.contains("+$43.76"), "cross-account delta: {rollup}");

        // Then each account's own detail, headed and subtotalled.
        assert!(
            b.contains(&format!("ACCOUNT {FIRM_ACCOUNT} — $133.76")),
            "{b}"
        );
        assert!(
            b.contains(&format!("ACCOUNT {SECOND_ACCOUNT} — $50.00")),
            "{b}"
        );
        assert!(b.contains("side-project"), "second account's project: {b}");
        assert!(
            b.find(FIRM_ACCOUNT) < b.find("side-project"),
            "accounts render in configured order: {b}"
        );
    }

    #[test]
    fn single_account_digest_omits_the_cross_account_roll_up() {
        // With one account the roll-up would just restate the total, and the
        // per-account heading would be noise.
        let b = build_digest_email(&sample_report(), "ops@example.com").body;
        assert!(!b.contains("BY BILLING ACCOUNT"), "{b}");
        assert!(!b.contains("ACCOUNT 013469-2BBE03-532C72 —"), "{b}");
        // ...but the account is still named on the covered line.
        assert!(b.contains(&format!("COVERED (1): {FIRM_ACCOUNT}")), "{b}");
    }

    #[test]
    fn report_journaled_before_the_account_split_still_replays() {
        // Restate replays the query step from its journal. A record written by
        // a release that grouped only by service has no `accounts`; it must
        // deserialize rather than fail the invocation permanently.
        let legacy = serde_json::json!({
            "window_days": 30,
            "as_of": "2026-06-15T13:00:00Z",
            "current": [ { "service": "Kubernetes Engine", "cost": 114.44 } ],
            "prior": [],
        });
        let report: BillingDigestReport =
            serde_json::from_value(legacy).expect("legacy journal entry deserializes");
        assert!(report.accounts.is_empty());
        assert_eq!(report.project_count(), 0);
        // It renders the no-rows note — degraded for the one invocation that
        // spans the deploy, never a crash.
        let b = build_digest_email(&report, "ops@example.com").body;
        assert!(b.contains("BILLING ACCOUNTS COVERED (0): none"), "{b}");
        assert!(b.contains("No billing rows in the trailing window"), "{b}");
    }

    #[test]
    fn digest_starts_with_money_and_renders_30_day_cost_table() {
        let report = sample_report();
        assert_eq!(report.service_count(), 2);
        assert_eq!(dollars(report.cost_total()), "$133.76");

        let email = build_digest_email(&report, "ops@example.com");
        assert_eq!(email.to, "ops@example.com");
        assert!(email.subject.starts_with("💸 $133.76 30-day GCP cost"));

        let b = &email.body;
        assert!(b.starts_with("$133.76 30-day GCP cost"), "body: {b}");
        assert!(b.contains("GCP COST BY SERVICE (30 DAYS)"));
        assert!(b.contains("Service") && b.contains("Cost") && b.contains("vs prior"));
        assert!(b.contains("Kubernetes Engine"));
        assert!(b.contains("Cloud SQL"));
        assert!(!b.contains("Artifact Registry"));
        assert!(!b.contains("Cloud Trace"));
        assert!(!b.contains("Invoice"));
        assert!(b.contains("$114.44"));
        assert!(b.contains("n/a"));
        assert!(b.contains("TOTAL"));
        assert!(b.contains("$133.76"), "gross total missing: {b}");
        assert!(b.contains("+$14.44"), "trend delta: {b}");
        assert!(b.contains("+$33.76"), "total delta: {b}");

        assert!(!b.contains("Credit"));
        assert!(!b.contains("credit"));
        assert!(!b.contains("trial"));
        assert!(!b.contains("PROMOTION"));
        assert!(!b.contains("Console-only"));
        assert!(!b.contains("-$0.00"));

        // Branded HTML retained with the plain-text fallback.
        assert!(email.html_body.is_some());
    }

    #[test]
    fn dollars_and_signed_dollars_round_near_zero_cleanly() {
        assert_eq!(dollars(-0.004), "$0.00");
        assert_eq!(dollars(0.004), "$0.00");
        assert_eq!(signed_dollars(-0.004), "$0.00");
        assert_eq!(signed_dollars(0.004), "$0.00");
        assert_eq!(signed_dollars(0.005), "+$0.01");
        assert_eq!(signed_dollars(-0.005), "-$0.01");
    }

    #[test]
    fn digest_with_no_rows_says_so_instead_of_a_zero_table() {
        let mut report = sample_report();
        report.accounts[0].current = Vec::new();
        report.accounts[0].current_by_project = Vec::new();
        let email = build_digest_email(&report, "ops@example.com");
        assert!(email
            .body
            .contains("No billing rows in the trailing window"));
        // No misleading totals table, in either grouping.
        assert!(!email.body.contains("COST BY SERVICE"));
        assert!(!email.body.contains("COST BY PROJECT"));
    }

    #[test]
    fn config_skips_query_until_both_table_and_project_are_set() {
        // Nothing set → no query (clean no-op), default recipient + window.
        let none = DigestConfig::from_env(|_| None);
        assert!(none.query.is_none());
        assert_eq!(none.recipient, "nick@neonlaw.com");
        assert_eq!(none.window_days, 30);

        // Table without project is still a no-op (no crash-loop).
        let half =
            DigestConfig::from_env(|k| (k == "BILLING_EXPORT_TABLE").then(|| "p.ds.t".to_string()));
        assert!(half.query.is_none());

        // Both set → query configured; env overrides honored.
        let full = DigestConfig::from_env(|k| match k {
            "BILLING_EXPORT_TABLE" => Some("p.ds.t".into()),
            "BIGQUERY_PROJECT" => Some("test-proj".into()),
            "BILLING_DIGEST_NOTIFY_EMAIL" => Some("billing@neonlaw.com".into()),
            "BILLING_DIGEST_WINDOW_DAYS" => Some("7".into()),
            _ => None,
        });
        let query = full.query.expect("both set → query configured");
        assert_eq!(query.tables, vec!["p.ds.t".to_string()]);
        assert_eq!(query.project, "test-proj");
        assert_eq!(full.recipient, "billing@neonlaw.com");
        assert_eq!(full.window_days, 7);
    }

    #[test]
    fn config_reads_one_export_table_per_billing_account() {
        // A comma-separated value is one table per account, in order.
        let multi = DigestConfig::from_env(|k| match k {
            "BILLING_EXPORT_TABLE" => Some("a.ds.one, b.ds.two".into()),
            "BIGQUERY_PROJECT" => Some("test-proj".into()),
            _ => None,
        });
        let query = multi.query.expect("configured");
        assert_eq!(
            query.tables,
            vec!["a.ds.one".to_string(), "b.ds.two".to_string()]
        );
        // One BigQuery project runs every account's query.
        assert_eq!(query.project, "test-proj");

        // A value that parses to no tables is the unconfigured no-op, not a
        // configured deploy that queries nothing.
        let blank = DigestConfig::from_env(|k| match k {
            "BILLING_EXPORT_TABLE" => Some(" , ".into()),
            "BIGQUERY_PROJECT" => Some("test-proj".into()),
            _ => None,
        });
        assert!(blank.query.is_none());
    }
}
