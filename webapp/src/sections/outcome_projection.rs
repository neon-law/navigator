//! The `outcome_projection` section: a contingent matter's projected profit.
//!
//! One bar per outcome — win at trial, settle, lose — with its net and odds,
//! the expected value, the costs at risk, where the odds came from (the
//! lawyer's estimate, the stage's Firm default, or the actual outcome), the
//! date of the estimate, and a stale flag when the matter changed stage
//! after it. Firm work product: `rules::dashboard::FIRM_WORK_PRODUCT` keeps
//! it on the lawyer lens, and the loader fills it on the lawyer lens only.
//!
//! Every value here is display-ready and wasm-safe; the server half
//! ([`derive`]) builds it from `store::matter_projections`.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Badge, BadgeTone, Empty, Panel};

/// One outcome's bar.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeBar {
    /// `Win at trial`, `Settle`, or `Lose`.
    pub label: String,
    /// The odds as a percentage, e.g. `60%`.
    pub odds: String,
    /// The odds in basis points, for the bar's width.
    pub odds_bps: i32,
    /// The outcome's net, e.g. `$49,950.00`; `None` when its odds carry no
    /// recovery.
    pub net: Option<String>,
    /// The net is a loss.
    pub negative: bool,
}

/// The `outcome_projection` section's data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutcomeProjectionView {
    /// The matter has a Contingency Term. Without one the section shows its
    /// empty state.
    pub contingent: bool,
    /// The projection could not be read.
    pub unavailable: bool,
    /// `Lawyer estimate`, `Stage default`, or `Actual outcome`; empty when
    /// the matter has no odds yet.
    pub source: String,
    /// When the current estimate was recorded, `YYYY-MM-DD`.
    pub estimated_on: Option<String>,
    /// The matter changed stage after the estimate.
    pub stale: bool,
    pub bars: Vec<OutcomeBar>,
    /// The expected net; `None` until the odds carry recoveries.
    pub expected_value: Option<String>,
    /// The lowest and highest net, e.g. `$-12,000.00 to $160,000.00`.
    pub range: Option<String>,
    /// The costs advanced that a loss would not recover.
    pub costs_at_risk: String,
}

/// The `outcome_projection` section.
#[component]
pub fn OutcomeProjectionSection(view: OutcomeProjectionView) -> Element {
    let note = if view.source.is_empty() {
        "Projected profit by outcome, net of costs advanced and time logged.".to_string()
    } else {
        format!(
            "Projected profit by outcome, net of costs advanced and time logged. Odds: {}.",
            view.source.to_lowercase()
        )
    };
    rsx! {
        section {
            class: "matter-section",
            "data-section": "outcome_projection",
            aria_labelledby: "section-outcome_projection-title",
            Panel {
                id: "section-outcome_projection".to_string(),
                title: "Outcome projection".to_string(),
                note,
                if view.unavailable {
                    div { class: "matter-section__empty",
                        Empty { title: "The projection is unavailable right now.".to_string() }
                    }
                } else if !view.contingent {
                    div { class: "matter-section__empty",
                        Empty { title: "This matter has no Contingency Term, so there is nothing to project.".to_string() }
                    }
                } else {
                    div { class: "matter-projection__meta",
                        if !view.source.is_empty() {
                            Badge { tone: BadgeTone::Default, "{view.source}" }
                        }
                        if let Some(on) = view.estimated_on.clone() {
                            span { class: "matter-projection__date", " Estimated {on}" }
                        }
                        if view.stale {
                            " "
                            Badge { tone: BadgeTone::Review, "Stale: the stage changed after this estimate" }
                        }
                    }
                    if view.bars.is_empty() {
                        p { class: "matter-section__empty", "No odds yet: record an Outcome Estimate." }
                    } else {
                        ul { class: "matter-projection",
                            for bar in view.bars.iter() {
                                li { class: "matter-projection__bar",
                                    key: "{bar.label}",
                                    strong { "{bar.label}" }
                                    " · {bar.odds}"
                                    meter {
                                        class: "matter-projection__odds",
                                        min: "0",
                                        max: "10000",
                                        value: "{bar.odds_bps}",
                                    }
                                    if let Some(net) = bar.net.clone() {
                                        span {
                                            class: if bar.negative { "matter-projection__net matter-projection__net--loss" } else { "matter-projection__net" },
                                            " {net}"
                                        }
                                    } else {
                                        span { class: "matter-projection__net matter-projection__gap", " no recovery estimated" }
                                    }
                                }
                            }
                        }
                    }
                    dl { class: "matter-projection__totals",
                        dt { "Expected value" }
                        dd { "{view.expected_value.clone().unwrap_or_else(|| \"Not yet estimated\".to_string())}" }
                        if let Some(range) = view.range.clone() {
                            dt { "Range" }
                            dd { "{range}" }
                        }
                        dt { "Costs at risk" }
                        dd { "{view.costs_at_risk}" }
                    }
                }
            }
        }
    }
}

/// The server half: the view from a matter's projection.
#[cfg(feature = "server")]
pub mod derive {
    use store::matter_projections::{MatterProjection, OddsSource};
    use store::outcome_estimates::Outcome;

    use super::{OutcomeBar, OutcomeProjectionView};
    use crate::portal_project_detail::format_usd;

    fn label(outcome: Outcome) -> &'static str {
        match outcome {
            Outcome::WinAtTrial => "Win at trial",
            Outcome::Settle => "Settle",
            Outcome::Lose => "Lose",
        }
    }

    /// Basis points as a percentage with at most one decimal, e.g. `33.3%`.
    fn percent(bps: i32) -> String {
        let tenths = bps / 10;
        if tenths % 10 == 0 {
            format!("{}%", tenths / 10)
        } else {
            format!("{}.{}%", tenths / 10, tenths % 10)
        }
    }

    /// Where the odds came from, as every surface labels it; empty with no
    /// odds yet.
    #[must_use]
    pub fn source_label(source: Option<OddsSource>) -> &'static str {
        match source {
            Some(OddsSource::Lawyer) => "Lawyer estimate",
            Some(OddsSource::StageDefault) => "Stage default",
            Some(OddsSource::Actual) => "Actual outcome",
            None => "",
        }
    }

    /// The section's view of `projection`.
    #[must_use]
    pub fn view(projection: &MatterProjection) -> OutcomeProjectionView {
        let MatterProjection::Contingent(projection) = projection else {
            return OutcomeProjectionView::default();
        };
        OutcomeProjectionView {
            contingent: true,
            unavailable: false,
            source: source_label(projection.source).to_string(),
            estimated_on: projection
                .estimate_recorded_at
                .map(|at| at.format("%Y-%m-%d").to_string()),
            stale: projection.stale,
            bars: projection
                .outcomes
                .iter()
                .map(|outcome| OutcomeBar {
                    label: label(outcome.outcome).to_string(),
                    odds: percent(outcome.probability_bps),
                    odds_bps: outcome.probability_bps,
                    net: outcome.net_cents.map(format_usd),
                    negative: outcome.net_cents.is_some_and(|net| net < 0),
                })
                .collect(),
            expected_value: projection.expected_net_cents.map(format_usd),
            range: projection
                .net_range_cents
                .map(|(low, high)| format!("{} to {}", format_usd(low), format_usd(high))),
            costs_at_risk: format_usd(projection.costs_at_risk_cents),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{percent, view};
        use store::matter_projections::{
            project_contingent, CertainProjection, ContingentInputs, MatterProjection, Odds,
            PositionedTier,
        };
        use store::outcome_estimates::{OutcomeOdds, OutcomeSet};

        fn worked_example() -> MatterProjection {
            MatterProjection::Contingent(project_contingent(&ContingentInputs {
                costs_deducted: store::contingency_terms::CostsDeducted::AfterFee,
                tiers: vec![
                    PositionedTier {
                        stage_position: 1,
                        fee_bps: 3_330,
                    },
                    PositionedTier {
                        stage_position: 5,
                        fee_bps: 4_000,
                    },
                ],
                current_stage_position: Some(3),
                odds: Odds::Lawyer(OutcomeSet {
                    win_at_trial: OutcomeOdds {
                        probability_bps: 1_500,
                        gross_recovery_cents: 40_000_000,
                    },
                    settle: OutcomeOdds {
                        probability_bps: 6_000,
                        gross_recovery_cents: 15_000_000,
                    },
                    lose: OutcomeOdds {
                        probability_bps: 2_500,
                        gross_recovery_cents: 0,
                    },
                }),
                costs_advanced_cents: 1_200_000,
                time_cost_cents: 0,
            }))
        }

        #[test]
        fn the_view_reads_the_projection_in_dollars_and_percentages() {
            let view = view(&worked_example());
            assert!(view.contingent);
            assert_eq!(view.source, "Lawyer estimate");
            let odds: Vec<_> = view.bars.iter().map(|bar| bar.odds.as_str()).collect();
            assert_eq!(odds, ["15%", "60%", "25%"]);
            assert_eq!(view.bars[2].net.as_deref(), Some("$-12,000.00"));
            assert!(view.bars[2].negative);
            assert_eq!(view.expected_value.as_deref(), Some("$50,970.00"));
            assert_eq!(view.costs_at_risk, "$12,000.00");
            assert_eq!(percent(3_330), "33.3%");
        }

        #[test]
        fn an_hourly_matter_has_no_projection_to_show() {
            let view = view(&MatterProjection::Certain(CertainProjection {
                wip_cents: 1,
                outstanding_cents: 2,
            }));
            assert!(!view.contingent);
            assert!(view.bars.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OutcomeBar, OutcomeProjectionView};
    use crate::sections::tests::render;
    use crate::sections::SectionView;

    #[test]
    fn the_empty_state_names_the_missing_contingency_term() {
        let html = render(SectionView::OutcomeProjection(
            OutcomeProjectionView::default(),
        ));
        assert!(html.contains("no Contingency Term"), "{html}");
    }

    #[test]
    fn a_stale_estimate_is_flagged_beside_its_date_and_source() {
        let html = render(SectionView::OutcomeProjection(OutcomeProjectionView {
            contingent: true,
            source: "Lawyer estimate".into(),
            estimated_on: Some("2026-11-01".into()),
            stale: true,
            bars: vec![OutcomeBar {
                label: "Settle".into(),
                odds: "60%".into(),
                odds_bps: 6_000,
                net: Some("$49,950.00".into()),
                negative: false,
            }],
            expected_value: Some("$50,970.00".into()),
            range: None,
            costs_at_risk: "$12,000.00".into(),
            unavailable: false,
        }));
        for expected in [
            "Lawyer estimate",
            "Estimated 2026-11-01",
            "Stale",
            "$49,950.00",
            "$50,970.00",
            "$12,000.00",
        ] {
            assert!(html.contains(expected), "{expected}: {html}");
        }
    }
}
