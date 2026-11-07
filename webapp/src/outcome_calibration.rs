//! The Firm show page's Outcome Calibration panel, beside
//! [`crate::projected_cash::FirmProjectedCash`], for Admin and Owner
//! (`store::outcome_calibration::firm_calibration`).
//!
//! Its view has no field that could carry a Project code, matter name,
//! client name, or invoice id: every label is a lead lawyer's name, a Case
//! Stage's name, `Unassigned`, or an odds band. Every value is
//! display-ready text formatted from integer basis points and cents.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// One odds band's predicted and actual win and settle rates.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct BandView {
    /// `20–40%`.
    pub label: String,
    pub win_matters: usize,
    pub win_predicted: String,
    pub win_actual: String,
    pub settle_matters: usize,
    pub settle_predicted: String,
    pub settle_actual: String,
}

/// One lead lawyer's or Case Stage's score.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct GroupView {
    /// A lead lawyer's name, a Case Stage's name, or `Unassigned`.
    pub label: String,
    pub matters: usize,
    pub brier: String,
    pub recovery_error: String,
}

/// The scores, once enough matters have resolved.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct CalibrationReportView {
    /// `0.1834`.
    pub brier: String,
    pub recovery_error: String,
    pub absolute_recovery_error: String,
    pub bands: Vec<BandView>,
    pub by_lead_lawyer: Vec<GroupView>,
    pub by_case_stage: Vec<GroupView>,
}

/// The Firm show page's Outcome Calibration.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct OutcomeCalibrationView {
    /// The store could not answer.
    #[serde(default)]
    pub unavailable: bool,
    pub lookback_days: u64,
    pub minimum_matters: usize,
    pub scored_matters: usize,
    pub unscored_matters: usize,
    pub report: Option<CalibrationReportView>,
}

#[component]
fn GroupTable(heading: String, groups: Vec<GroupView>) -> Element {
    rsx! {
        h3 { "{heading}" }
        div { class: "nav-table-wrap",
            table { class: "nav-table",
                thead {
                    tr {
                        th { scope: "col", "Name" }
                        th { scope: "col", "Matters" }
                        th { scope: "col", "Brier score" }
                        th { scope: "col", "Mean recovery error" }
                    }
                }
                tbody {
                    for group in groups.iter() {
                        tr { key: "{group.label}",
                            td { "{group.label}" }
                            td { "{group.matters}" }
                            td { "{group.brier}" }
                            td { "{group.recovery_error}" }
                        }
                    }
                }
            }
        }
    }
}

/// How well the odds in force before resolution predicted what happened:
/// rates by odds band, the Brier score, and the recovery error, by lead
/// lawyer and Case Stage. Names no matter.
#[component]
pub fn FirmOutcomeCalibration(view: OutcomeCalibrationView) -> Element {
    rsx! {
        section { id: "firm-outcome-calibration",
            h2 { "Outcome calibration" }
            p { class: "nav-muted",
                "Resolved contingent matters, scored on the Outcome Estimate in force {view.lookback_days} days before each resolved."
            }
            if view.unavailable {
                p { class: "nav-muted", "Calibration is unavailable right now." }
            } else if let Some(report) = view.report.clone() {
                dl { class: "lawyer-pipeline__totals",
                    dt { "Matters scored" }
                    dd { "{view.scored_matters}" }
                    dt { "Brier score" }
                    dd { "{report.brier}" }
                    dt { "Mean recovery error" }
                    dd { "{report.recovery_error}" }
                    dt { "Mean absolute recovery error" }
                    dd { "{report.absolute_recovery_error}" }
                }
                p { class: "nav-muted",
                    "A Brier score runs from 0 (every outcome called with certainty) to 2 (certain and wrong). A positive recovery error means the odds expected more than was recovered."
                }
                h3 { "Predicted against actual, by odds band" }
                div { class: "nav-table-wrap",
                    table { class: "nav-table",
                        thead {
                            tr {
                                th { scope: "col", "Odds band" }
                                th { scope: "col", "Win: matters" }
                                th { scope: "col", "Win: predicted" }
                                th { scope: "col", "Win: actual" }
                                th { scope: "col", "Settle: matters" }
                                th { scope: "col", "Settle: predicted" }
                                th { scope: "col", "Settle: actual" }
                            }
                        }
                        tbody {
                            for band in report.bands.iter() {
                                tr { key: "{band.label}",
                                    th { scope: "row", "{band.label}" }
                                    td { "{band.win_matters}" }
                                    td { "{band.win_predicted}" }
                                    td { "{band.win_actual}" }
                                    td { "{band.settle_matters}" }
                                    td { "{band.settle_predicted}" }
                                    td { "{band.settle_actual}" }
                                }
                            }
                        }
                    }
                }
                GroupTable { heading: "By lead lawyer".to_string(), groups: report.by_lead_lawyer.clone() }
                GroupTable { heading: "By Case Stage".to_string(), groups: report.by_case_stage.clone() }
            } else {
                p { class: "nav-muted",
                    "Calibration needs at least {view.minimum_matters} resolved matters with an estimate in force; {view.scored_matters} so far."
                }
            }
            if !view.unavailable && view.unscored_matters > 0 {
                p { class: "nav-muted",
                    "Resolved matters with no estimate {view.lookback_days} days before resolving: {view.unscored_matters}."
                }
            }
        }
    }
}

/// The server half: views from `store::outcome_calibration`.
#[cfg(feature = "server")]
pub mod derive {
    use store::outcome_calibration::{CalibrationGroup, CalibrationReport, OutcomeCalibration};

    use super::{BandView, CalibrationReportView, GroupView, OutcomeCalibrationView};
    use crate::portal_project_detail::format_usd;

    /// `4250` → `42.5%`.
    fn percent(bps: i64) -> String {
        format!("{}.{}%", bps / 100, (bps % 100).abs() / 10)
    }

    /// `1834` → `0.1834`.
    fn brier(bps: i64) -> String {
        format!("{}.{:04}", bps / 10_000, bps % 10_000)
    }

    fn rate(bps: Option<i64>) -> String {
        bps.map_or_else(|| "—".to_string(), percent)
    }

    fn groups(groups: &[CalibrationGroup]) -> Vec<GroupView> {
        groups
            .iter()
            .map(|group| GroupView {
                label: group.label.clone(),
                matters: group.matters,
                brier: brier(group.brier_bps),
                recovery_error: format_usd(group.mean_recovery_error_cents),
            })
            .collect()
    }

    fn report(report: &CalibrationReport) -> CalibrationReportView {
        CalibrationReportView {
            brier: brier(report.brier_bps),
            recovery_error: format_usd(report.mean_recovery_error_cents),
            absolute_recovery_error: format_usd(report.mean_absolute_recovery_error_cents),
            bands: report
                .bands
                .iter()
                .map(|band| BandView {
                    label: format!("{}–{}%", band.low_bps / 100, band.high_bps / 100),
                    win_matters: band.win_at_trial.matters,
                    win_predicted: rate(band.win_at_trial.predicted_bps),
                    win_actual: rate(band.win_at_trial.actual_bps),
                    settle_matters: band.settle.matters,
                    settle_predicted: rate(band.settle.predicted_bps),
                    settle_actual: rate(band.settle.actual_bps),
                })
                .collect(),
            by_lead_lawyer: groups(&report.by_lead_lawyer),
            by_case_stage: groups(&report.by_case_stage),
        }
    }

    /// The panel's view of `calibration`.
    #[must_use]
    pub fn firm(calibration: &OutcomeCalibration) -> OutcomeCalibrationView {
        OutcomeCalibrationView {
            unavailable: false,
            lookback_days: calibration.lookback_days,
            minimum_matters: calibration.minimum_matters,
            scored_matters: calibration.scored_matters,
            unscored_matters: calibration.unscored_matters,
            report: calibration.report.as_ref().map(report),
        }
    }

    /// The Firm's calibration, read as `viewer`. A store that cannot answer
    /// marks the panel unavailable rather than failing the page.
    pub async fn load_firm(
        surreal: &store::surreal::SurrealDb,
        viewer: Option<uuid::Uuid>,
        firm_id: uuid::Uuid,
    ) -> OutcomeCalibrationView {
        let unavailable = OutcomeCalibrationView {
            unavailable: true,
            ..OutcomeCalibrationView::default()
        };
        let Some(viewer) = viewer else {
            return unavailable;
        };
        let Ok(pg) = store::postgres::PostgresDb::from_env().await else {
            return unavailable;
        };
        match store::outcome_calibration::firm_calibration(surreal, &pg, viewer, firm_id).await {
            Ok(calibration) => firm(&calibration),
            Err(error) => {
                tracing::warn!(error = %error, "firm page: outcome calibration failed");
                unavailable
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{brier, percent};

        #[test]
        fn basis_points_format_without_floats() {
            assert_eq!(percent(4_250), "42.5%");
            assert_eq!(percent(10_000), "100.0%");
            assert_eq!(percent(5), "0.0%");
            assert_eq!(brier(1_834), "0.1834");
            assert_eq!(brier(20_000), "2.0000");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BandView, CalibrationReportView, FirmOutcomeCalibration, FirmOutcomeCalibrationProps,
        GroupView, OutcomeCalibrationView,
    };
    use dioxus::prelude::VirtualDom;

    fn render(view: OutcomeCalibrationView) -> String {
        let mut dom = VirtualDom::new_with_props(
            FirmOutcomeCalibration,
            FirmOutcomeCalibrationProps { view },
        );
        dom.rebuild_in_place();
        dioxus_ssr::render(&dom)
    }

    fn group(label: &str) -> GroupView {
        GroupView {
            label: label.to_string(),
            matters: 4,
            brier: "0.2500".to_string(),
            recovery_error: "$-1,000.00".to_string(),
        }
    }

    fn reported() -> OutcomeCalibrationView {
        OutcomeCalibrationView {
            lookback_days: 30,
            minimum_matters: 10,
            scored_matters: 12,
            unscored_matters: 2,
            report: Some(CalibrationReportView {
                brier: "0.1834".to_string(),
                recovery_error: "$12,500.00".to_string(),
                absolute_recovery_error: "$40,000.00".to_string(),
                bands: vec![BandView {
                    label: "40–60%".to_string(),
                    win_matters: 3,
                    win_predicted: "50.0%".to_string(),
                    win_actual: "66.6%".to_string(),
                    settle_matters: 0,
                    settle_predicted: "—".to_string(),
                    settle_actual: "—".to_string(),
                }],
                by_lead_lawyer: vec![group("Lawyer One"), group("Unassigned")],
                by_case_stage: vec![group("Discovery")],
            }),
            ..OutcomeCalibrationView::default()
        }
    }

    #[test]
    fn the_panel_shows_bands_scores_and_groups() {
        let html = render(reported());
        for text in [
            r#"id="firm-outcome-calibration""#,
            "in force 30 days before",
            "0.1834",
            "$12,500.00",
            "40–60%",
            "66.6%",
            "By lead lawyer",
            "By Case Stage",
            "Discovery",
            "Unassigned",
            "no estimate 30 days before resolving: 2.",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
    }

    #[test]
    fn below_the_minimum_or_unavailable_the_panel_says_so() {
        let few = render(OutcomeCalibrationView {
            lookback_days: 30,
            minimum_matters: 10,
            scored_matters: 3,
            ..OutcomeCalibrationView::default()
        });
        assert!(
            few.contains("needs at least 10 resolved matters with an estimate in force; 3 so far."),
            "{few}"
        );
        assert!(!few.contains("Brier score"), "{few}");
        let down = render(OutcomeCalibrationView {
            unavailable: true,
            ..OutcomeCalibrationView::default()
        });
        assert!(
            down.contains("Calibration is unavailable right now."),
            "{down}"
        );
    }

    /// ENG-1251 privacy: the view has no field to carry a Project code,
    /// matter name, client name, or invoice id, and the rendered panel links
    /// to no matter.
    #[test]
    fn no_project_link_or_invoice_id_appears_in_the_panel() {
        let html = render(reported());
        for forbidden in ["/app/projects/", "xero", "INV-", "project_id", "href"] {
            assert!(
                !html.to_lowercase().contains(&forbidden.to_lowercase()),
                "{forbidden} leaked into: {html}"
            );
        }
    }
}
