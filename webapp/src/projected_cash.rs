//! Projected contingency cash across matters, drawn as server-rendered
//! inline SVG with no charting library, after the status pie in
//! `webapp::lawyer_dashboard` and `webapp::firm_invoice_graphs`.
//!
//! - [`PipelineCard`]: the lawyer workbench's Contingency Pipeline over the
//!   caller's own matters (`store::projected_cash::lawyer_pipeline`).
//! - [`FirmProjectedCash`]: the Firm show page's rollup beside the invoice
//!   graphs, for Admin and Owner (`store::projected_cash::firm_rollup`). Its
//!   view has no field that could carry a Project code, matter name, client
//!   name, or invoice id: every label is a brand's name, a lead lawyer's
//!   name, `Unassigned`, or a quarter.
//!
//! Every value is display-ready and wasm-safe; the server half formats it
//! from `store::projected_cash`. Firm work product: none of it reaches a
//! client surface.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Badge, BadgeTone};

/// One bar in a quarter's group.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct BarView {
    /// The CSS modifier naming the series, e.g. `projected`.
    pub series: String,
    pub cents: i64,
    /// The bar's tooltip and accessible text, e.g. `2027 Q1 projected: $62,970.00`.
    pub title: String,
}

/// One quarter's group of bars.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct QuarterBarsView {
    /// `2027 Q1`, or `Later`.
    pub label: String,
    pub bars: Vec<BarView>,
}

const PLOT_HEIGHT: i64 = 120;
const LABEL_BAND: i64 = 20;
const BAR_WIDTH: i64 = 18;
const BAR_GAP: i64 = 3;
const GROUP_GAP: i64 = 18;

/// A bar's height out of [`PLOT_HEIGHT`], in integer maths.
fn bar_height(cents: i64, max_cents: i64) -> i64 {
    if max_cents <= 0 || cents <= 0 {
        return 0;
    }
    cents.saturating_mul(PLOT_HEIGHT) / max_cents
}

/// Vertical grouped bars, one group per quarter, every bar scaled to the
/// chart's largest value.
#[component]
pub fn QuarterChart(quarters: Vec<QuarterBarsView>, aria_label: String) -> Element {
    let max_cents = quarters
        .iter()
        .flat_map(|quarter| quarter.bars.iter().map(|bar| bar.cents))
        .max()
        .unwrap_or(0);
    let per_group = i64::try_from(quarters.first().map_or(1, |q| q.bars.len().max(1))).unwrap_or(1);
    let group_width = per_group * (BAR_WIDTH + BAR_GAP) - BAR_GAP + GROUP_GAP;
    let width = (group_width * i64::try_from(quarters.len()).unwrap_or(0)).max(group_width);
    let height = PLOT_HEIGHT + LABEL_BAND;
    rsx! {
        svg {
            class: "projected-cash-chart",
            view_box: "0 0 {width} {height}",
            width: "{width}",
            height: "{height}",
            role: "img",
            "aria-label": "{aria_label}",
            for (index , quarter) in quarters.iter().enumerate() {
                {
                    let x_group = i64::try_from(index).unwrap_or(0) * group_width;
                    let label_x = x_group + (group_width - GROUP_GAP) / 2;
                    rsx! {
                        g { class: "projected-cash-quarter", key: "{quarter.label}",
                            for (slot , bar) in quarter.bars.iter().enumerate() {
                                {
                                    let bar_h = bar_height(bar.cents, max_cents);
                                    let x = x_group + i64::try_from(slot).unwrap_or(0) * (BAR_WIDTH + BAR_GAP);
                                    let y = PLOT_HEIGHT - bar_h;
                                    rsx! {
                                        rect {
                                            x: "{x}",
                                            y: "{y}",
                                            width: "{BAR_WIDTH}",
                                            height: "{bar_h}",
                                            class: "projected-cash-bar projected-cash-bar--{bar.series}",
                                            title { "{bar.title}" }
                                        }
                                    }
                                }
                            }
                            text {
                                x: "{label_x}",
                                y: "{height - 4}",
                                text_anchor: "middle",
                                class: "projected-cash-label",
                                "{quarter.label}"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One matter on the pipeline card. The caller already sees the matter.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct PipelineRowView {
    pub code: String,
    pub name: String,
    /// `Lawyer estimate`, `Stage default`, or empty with no odds yet.
    pub source: String,
    /// The expected net; `None` until the odds carry recoveries.
    pub expected_value: Option<String>,
    /// The lowest to highest net.
    pub range: Option<String>,
    pub stale: bool,
}

/// The workbench's Contingency Pipeline card.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct PipelineCardView {
    /// The store could not answer.
    #[serde(default)]
    pub unavailable: bool,
    pub expected_fees: String,
    pub expected_net: String,
    pub net_range: String,
    pub costs_at_risk: String,
    pub quarters: Vec<QuarterBarsView>,
    /// Highest expected net first; unestimated matters last.
    pub matters: Vec<PipelineRowView>,
}

/// The Contingency Pipeline card on `/app/lawyer`: expected fees, costs at
/// risk, expected cash by quarter, the caller's contingent matters ranked by
/// expected value, and the ones whose estimate went stale.
#[component]
pub fn PipelineCard(view: PipelineCardView) -> Element {
    let stale: Vec<PipelineRowView> = view.matters.iter().filter(|m| m.stale).cloned().collect();
    rsx! {
        section { id: "lawyer-pipeline-card", class: "lawyer-pipeline",
            h2 { "Contingency pipeline" }
            p { class: "nav-muted",
                "Expected cash from your open contingent matters, from the odds on each. A range, never a promise."
            }
            if view.unavailable {
                p { class: "nav-muted", "The pipeline is unavailable right now." }
            } else if view.matters.is_empty() {
                p { class: "nav-muted", "No open contingent matters in your workload." }
            } else {
                dl { class: "lawyer-pipeline__totals",
                    dt { "Expected fees" }
                    dd { "{view.expected_fees}" }
                    dt { "Expected profit" }
                    dd { "{view.expected_net}" }
                    dt { "Range" }
                    dd { "{view.net_range}" }
                    dt { "Costs at risk" }
                    dd { "{view.costs_at_risk}" }
                }
                if view.quarters.is_empty() {
                    p { class: "nav-muted", "No estimate names a resolution date yet." }
                } else {
                    h3 { "Expected cash by quarter" }
                    QuarterChart {
                        quarters: view.quarters.clone(),
                        aria_label: "Expected cash by quarter of expected resolution".to_string(),
                    }
                }
                h3 { "By expected value" }
                div { class: "nav-table-wrap",
                    table { class: "nav-table",
                        thead {
                            tr {
                                th { scope: "col", "Matter" }
                                th { scope: "col", "Expected value" }
                                th { scope: "col", "Range" }
                                th { scope: "col", "Odds" }
                            }
                        }
                        tbody {
                            for row in view.matters.iter() {
                                tr { key: "{row.code}",
                                    td { a { href: "/app/projects/{row.code}", "{row.name}" } }
                                    td { "{row.expected_value.clone().unwrap_or_else(|| \"Not yet estimated\".to_string())}" }
                                    td { "{row.range.clone().unwrap_or_default()}" }
                                    td {
                                        if row.source.is_empty() {
                                            "None yet"
                                        } else {
                                            "{row.source}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if !stale.is_empty() {
                    h3 { "Stale estimates" }
                    ul { class: "lawyer-pipeline__stale",
                        for row in stale.iter() {
                            li { key: "{row.code}",
                                Badge { tone: BadgeTone::Review, "Stale" }
                                " "
                                a { href: "/app/projects/{row.code}", "{row.name}" }
                                ": the stage changed after the estimate."
                            }
                        }
                    }
                }
            }
        }
    }
}

/// One brand's or lead lawyer's expected cash.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct CashGroupView {
    /// A brand's display name, a lead lawyer's display name, or `Unassigned`.
    pub label: String,
    pub cash: String,
    pub matters: usize,
}

/// The Firm show page's projected contingency cash.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct FirmProjectedCashView {
    /// The store could not answer.
    #[serde(default)]
    pub unavailable: bool,
    pub expected_cash: String,
    pub cash_range: String,
    /// Projected, invoiced, and paid bars per quarter.
    pub quarters: Vec<QuarterBarsView>,
    pub by_brand: Vec<CashGroupView>,
    pub by_lead_lawyer: Vec<CashGroupView>,
    /// The largest single matter's share, e.g. `42%`, past the threshold.
    pub concentration: Option<String>,
    pub unestimated_matters: usize,
}

#[component]
fn CashGroupTable(heading: String, groups: Vec<CashGroupView>) -> Element {
    rsx! {
        h3 { "{heading}" }
        div { class: "nav-table-wrap",
            table { class: "nav-table",
                thead {
                    tr {
                        th { scope: "col", "Name" }
                        th { scope: "col", "Expected cash" }
                        th { scope: "col", "Matters" }
                    }
                }
                tbody {
                    for group in groups.iter() {
                        tr { key: "{group.label}",
                            td { "{group.label}" }
                            td { "{group.cash}" }
                            td { "{group.matters}" }
                        }
                    }
                }
            }
        }
    }
}

/// The Firm show page's projected contingency cash: expected cash by
/// quarter beside the USD invoiced and paid, by brand and by lead lawyer,
/// and a warning when one matter holds too much of it. Names no matter.
#[component]
pub fn FirmProjectedCash(view: FirmProjectedCashView) -> Element {
    let any = !view.by_brand.is_empty();
    rsx! {
        section { id: "firm-projected-cash",
            h2 { "Projected contingency cash" }
            p { class: "nav-muted",
                "Expected fees and costs recovered on open contingent matters, by quarter of expected resolution, beside the USD invoiced and paid on invoices issued each quarter."
            }
            if view.unavailable {
                p { class: "nav-muted", "The projection is unavailable right now." }
            } else {
                if any {
                    dl { class: "lawyer-pipeline__totals",
                        dt { "Expected cash" }
                        dd { "{view.expected_cash}" }
                        dt { "Range" }
                        dd { "{view.cash_range}" }
                    }
                } else {
                    p { class: "nav-muted", "No open contingent matter carries an estimate yet." }
                }
                if let Some(share) = view.concentration.clone() {
                    p { class: "firm-projected-cash__warning", role: "note",
                        Badge { tone: BadgeTone::Review, "Concentration" }
                        " One matter holds {share} of the Firm's expected cash."
                    }
                }
                if view.unestimated_matters > 0 {
                    p { class: "nav-muted",
                        "Open contingent matters with no recovery estimated yet: {view.unestimated_matters}."
                    }
                }
                QuarterChart {
                    quarters: view.quarters.clone(),
                    aria_label: "Projected cash, invoiced, and paid by quarter".to_string(),
                }
                ul { class: "projected-cash-legend",
                    li { span { class: "projected-cash-key projected-cash-key--projected", aria_hidden: "true" } "Projected" }
                    li { span { class: "projected-cash-key projected-cash-key--invoiced", aria_hidden: "true" } "Invoiced" }
                    li { span { class: "projected-cash-key projected-cash-key--paid", aria_hidden: "true" } "Paid" }
                }
                if any {
                    CashGroupTable { heading: "By brand".to_string(), groups: view.by_brand.clone() }
                    CashGroupTable { heading: "By lead lawyer".to_string(), groups: view.by_lead_lawyer.clone() }
                }
            }
        }
    }
}

/// The server half: views from `store::projected_cash`.
#[cfg(feature = "server")]
pub mod derive {
    use store::projected_cash::{CashGroup, FirmProjectedCash, LawyerPipeline};

    use super::{
        BarView, CashGroupView, FirmProjectedCashView, PipelineCardView, PipelineRowView,
        QuarterBarsView,
    };
    use crate::portal_project_detail::format_usd;
    use crate::sections::outcome_projection::derive::source_label;

    fn range((low, high): (i64, i64)) -> String {
        format!("{} to {}", format_usd(low), format_usd(high))
    }

    /// The card's view of `pipeline`.
    #[must_use]
    pub fn pipeline(pipeline: &LawyerPipeline) -> PipelineCardView {
        PipelineCardView {
            unavailable: false,
            expected_fees: format_usd(pipeline.expected_fee_cents),
            expected_net: format_usd(pipeline.expected_net_cents),
            net_range: range(pipeline.net_range_cents),
            costs_at_risk: format_usd(pipeline.costs_at_risk_cents),
            quarters: pipeline
                .by_quarter
                .iter()
                .map(|quarter| QuarterBarsView {
                    label: quarter.label.clone(),
                    bars: vec![BarView {
                        series: "projected".to_string(),
                        cents: quarter.expected_cash_cents,
                        title: format!(
                            "{} expected: {}",
                            quarter.label,
                            format_usd(quarter.expected_cash_cents)
                        ),
                    }],
                })
                .collect(),
            matters: pipeline
                .matters
                .iter()
                .map(|matter| PipelineRowView {
                    code: matter.code.clone(),
                    name: matter.name.clone(),
                    source: source_label(matter.source).to_string(),
                    expected_value: matter.expected_net_cents.map(format_usd),
                    range: matter.net_range_cents.map(range),
                    stale: matter.stale,
                })
                .collect(),
        }
    }

    /// The caller's pipeline over `projects`, the workbench's KPI
    /// collection. A store that cannot answer marks the card unavailable
    /// rather than failing the workbench.
    pub async fn load_pipeline(
        surreal: &store::surreal::SurrealDb,
        viewer: Option<uuid::Uuid>,
        projects: &[store::projects::Project],
    ) -> PipelineCardView {
        let unavailable = PipelineCardView {
            unavailable: true,
            ..PipelineCardView::default()
        };
        let Some(viewer) = viewer else {
            return PipelineCardView::default();
        };
        let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
            return unavailable;
        };
        let today = chrono::Utc::now().date_naive();
        match store::projected_cash::lawyer_pipeline(surreal, &pg, viewer, projects, today).await {
            Ok(found) => pipeline(&found),
            Err(error) => {
                tracing::warn!(error = %error, "workbench: contingency pipeline failed");
                unavailable
            }
        }
    }
    fn groups(groups: &[CashGroup]) -> Vec<CashGroupView> {
        groups
            .iter()
            .map(|group| CashGroupView {
                label: group.label.clone(),
                cash: format_usd(group.expected_cash_cents),
                matters: group.matters,
            })
            .collect()
    }

    fn bar(quarter: &str, series: &str, cents: i64) -> BarView {
        BarView {
            series: series.to_string(),
            cents,
            title: format!("{quarter} {series}: {}", format_usd(cents)),
        }
    }

    /// The Firm section's view of `rollup`.
    #[must_use]
    pub fn firm(rollup: &FirmProjectedCash) -> FirmProjectedCashView {
        FirmProjectedCashView {
            unavailable: false,
            expected_cash: format_usd(rollup.expected_cash_cents),
            cash_range: range(rollup.cash_range_cents),
            quarters: rollup
                .quarters
                .iter()
                .map(|quarter| QuarterBarsView {
                    label: quarter.label.clone(),
                    bars: vec![
                        bar(&quarter.label, "projected", quarter.projected_cash_cents),
                        bar(&quarter.label, "invoiced", quarter.invoiced_cents),
                        bar(&quarter.label, "paid", quarter.paid_cents),
                    ],
                })
                .collect(),
            by_brand: groups(&rollup.by_brand),
            by_lead_lawyer: groups(&rollup.by_lead_lawyer),
            concentration: rollup
                .concentration_bps
                .map(|bps| format!("{}%", bps / 100)),
            unestimated_matters: rollup.unestimated_matters,
        }
    }

    /// The Firm's projected cash, read as `viewer`. A store that cannot
    /// answer marks the section unavailable rather than failing the page.
    pub async fn load_firm(
        surreal: &store::surreal::SurrealDb,
        viewer: Option<uuid::Uuid>,
        firm_id: uuid::Uuid,
    ) -> FirmProjectedCashView {
        let unavailable = FirmProjectedCashView {
            unavailable: true,
            ..FirmProjectedCashView::default()
        };
        let Some(viewer) = viewer else {
            return unavailable;
        };
        let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
            return unavailable;
        };
        let today = chrono::Utc::now().date_naive();
        match store::projected_cash::firm_rollup(surreal, &pg, viewer, firm_id, today).await {
            Ok(rollup) => firm(&rollup),
            Err(error) => {
                tracing::warn!(error = %error, "firm page: projected cash failed");
                unavailable
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BarView, CashGroupView, FirmProjectedCash, FirmProjectedCashProps, FirmProjectedCashView,
        PipelineCard, PipelineCardProps, PipelineCardView, PipelineRowView, QuarterBarsView,
    };
    use dioxus::prelude::VirtualDom;

    fn render(view: PipelineCardView) -> String {
        let mut dom = VirtualDom::new_with_props(PipelineCard, PipelineCardProps { view });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn row(code: &str, expected: Option<&str>, stale: bool) -> PipelineRowView {
        PipelineRowView {
            code: code.to_string(),
            name: format!("{code} matter"),
            source: if expected.is_some() {
                "Lawyer estimate".to_string()
            } else {
                "Stage default".to_string()
            },
            expected_value: expected.map(str::to_string),
            range: expected.map(|_| "$-12,000.00 to $160,000.00".to_string()),
            stale,
        }
    }

    fn quarter(label: &str, cents: i64) -> QuarterBarsView {
        QuarterBarsView {
            label: label.to_string(),
            bars: vec![BarView {
                series: "projected".to_string(),
                cents,
                title: format!("{label} expected: ${}", cents / 100),
            }],
        }
    }

    #[test]
    fn the_card_shows_totals_a_quarter_chart_ranked_matters_and_stale_ones() {
        let html = render(PipelineCardView {
            expected_fees: "$53,970.00".to_string(),
            expected_net: "$50,970.00".to_string(),
            net_range: "$-12,000.00 to $160,000.00".to_string(),
            costs_at_risk: "$12,000.00".to_string(),
            quarters: vec![quarter("2026 Q4", 100_000), quarter("2027 Q1", 50_000)],
            matters: vec![
                row("alpha", Some("$50,970.00"), true),
                row("bravo", None, false),
            ],
            ..PipelineCardView::default()
        });
        for text in [
            "Expected fees",
            "$53,970.00",
            "$-12,000.00 to $160,000.00",
            "Costs at risk",
            "$12,000.00",
            "2026 Q4 expected: $1000",
            "Not yet estimated",
            "Stage default",
            "Stale estimates",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        // The larger quarter fills the plot; the other is half of it.
        assert!(html.contains(r#"height="120""#), "{html}");
        assert!(html.contains(r#"height="60""#), "{html}");
        assert!(html.contains(r#"href="/app/projects/alpha""#), "{html}");
        let ranked = html.find("alpha matter").unwrap() < html.find("bravo matter").unwrap();
        assert!(ranked, "{html}");
    }

    #[test]
    fn an_empty_or_unavailable_pipeline_says_so_without_a_chart() {
        let empty = render(PipelineCardView::default());
        assert!(
            empty.contains("No open contingent matters in your workload."),
            "{empty}"
        );
        assert!(!empty.contains("<svg"), "{empty}");
        let down = render(PipelineCardView {
            unavailable: true,
            ..PipelineCardView::default()
        });
        assert!(
            down.contains("The pipeline is unavailable right now."),
            "{down}"
        );
    }

    fn render_firm(view: FirmProjectedCashView) -> String {
        let mut dom =
            VirtualDom::new_with_props(FirmProjectedCash, FirmProjectedCashProps { view });
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn group(label: &str) -> CashGroupView {
        CashGroupView {
            label: label.to_string(),
            cash: "$62,970.00".to_string(),
            matters: 1,
        }
    }

    fn firm_view() -> FirmProjectedCashView {
        let bars = |label: &str| QuarterBarsView {
            label: label.to_string(),
            bars: ["projected", "invoiced", "paid"]
                .iter()
                .map(|series| BarView {
                    series: (*series).to_string(),
                    cents: 1_000,
                    title: format!("{label} {series}: $10.00"),
                })
                .collect(),
        };
        FirmProjectedCashView {
            expected_cash: "$125,940.00".to_string(),
            cash_range: "$0.00 to $344,000.00".to_string(),
            quarters: vec![bars("2026 Q3"), bars("2026 Q4")],
            by_brand: vec![group("Neon Law")],
            by_lead_lawyer: vec![group("Lawyer One"), group("Unassigned")],
            concentration: Some("50%".to_string()),
            unestimated_matters: 2,
            ..FirmProjectedCashView::default()
        }
    }

    #[test]
    fn the_firm_section_shows_quarters_beside_invoices_groups_and_a_concentration_warning() {
        let html = render_firm(firm_view());
        for text in [
            r#"id="firm-projected-cash""#,
            "$125,940.00",
            "2026 Q4 projected: $10.00",
            "2026 Q4 invoiced: $10.00",
            "2026 Q4 paid: $10.00",
            "By brand",
            "By lead lawyer",
            "Unassigned",
            "One matter holds 50% of the Firm",
            "no recovery estimated yet: 2.",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
    }

    #[test]
    fn a_firm_with_no_estimates_says_so_and_raises_no_warning() {
        let html = render_firm(FirmProjectedCashView::default());
        assert!(
            html.contains("No open contingent matter carries an estimate yet."),
            "{html}"
        );
        assert!(!html.contains("Concentration"), "{html}");
        assert!(!html.contains("By brand"), "{html}");
    }

    /// ENG-1250 privacy: the view has no field to carry a Project code,
    /// matter name, client name, or invoice id, and the rendered section
    /// names no matter and links to none.
    #[test]
    fn no_project_code_matter_name_or_invoice_id_appears_in_the_firm_section() {
        let html = render_firm(firm_view());
        for forbidden in ["/app/projects/", "xero", "INV-", "project_id", "matter-"] {
            assert!(
                !html.to_lowercase().contains(&forbidden.to_lowercase()),
                "{forbidden} leaked into: {html}"
            );
        }
    }
}
