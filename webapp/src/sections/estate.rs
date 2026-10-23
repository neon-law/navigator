//! The estate plan console's sections: `instrument_set`, `signing_ceremony`,
//! `distribution_plan`, `administration_steps`, `fiduciary_roster`, and
//! `asset_schedule`.
//!
//! Every value here is display-ready and wasm-safe. The server half
//! ([`derive`]) builds them from the matter's Notations and the rows behind
//! the `estate` Module, already scoped to the lens: a hold is the firm's
//! drafting note, so only the lawyer lens reads its words, and a client reads
//! the Module's rows only while it is enabled.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::{Badge, BadgeTone, Empty, Panel};

/// One execution step of an instrument.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepView {
    pub label: String,
    /// `Done`, `In progress`, `Not started`, or `Declined`.
    pub state: String,
    /// When it was done, `YYYY-MM-DD`; empty otherwise.
    pub on: String,
}

/// What blocks an instrument, with where to resolve it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockerView {
    /// `Open term` or `Hold`.
    pub kind: String,
    pub text: String,
    pub href: String,
}

/// One estate instrument's card.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstrumentCard {
    pub title: String,
    /// The instrument's kind in words: `Trust`, `Will`, `Directive`.
    pub kind: String,
    /// `Drafting`, `Approved`, `At ceremony`, `Blocked`, or `Executed`.
    pub status: String,
    /// The formalities its workflow declares, in order.
    pub steps: Vec<StepView>,
    pub blockers: Vec<BlockerView>,
    /// The instrument's outline page.
    pub href: String,
}

/// One instrument at the signing ceremony.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeremonyInstrument {
    pub title: String,
    pub steps: Vec<StepView>,
}

/// The `signing_ceremony` section's data. No instruments is the empty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CeremonyView {
    /// The recorded date; empty when none is recorded.
    pub date: String,
    /// The recorded place; empty when none is recorded.
    pub place: String,
    pub instruments: Vec<CeremonyInstrument>,
}

/// One residuary share.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareView {
    pub beneficiary: String,
    /// The share as recorded, e.g. `50%`.
    pub share: String,
    /// The share in hundredths of a percent, when it reads as a number.
    pub basis_points: Option<u32>,
}

/// The `distribution_plan` section's data. No shares is the empty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DistributionView {
    /// The trust the shares come from.
    pub source: String,
    pub shares: Vec<ShareView>,
}

/// One stage of a workflow.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageView {
    pub label: String,
    /// `Done`, `Now`, `Next`, or `Later`.
    pub state: String,
}

/// The `administration_steps` section's data. No stages is the empty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagesView {
    /// The Notation whose workflow the stages are.
    pub notation: String,
    pub stages: Vec<StageView>,
}

/// Every estate section's data for one reader, built once per page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EstateViews {
    pub instruments: Vec<InstrumentCard>,
    pub ceremony: CeremonyView,
    pub distribution: DistributionView,
    pub stages: StagesView,
    pub fiduciaries: Vec<FiduciaryRow>,
    pub assets: Vec<AssetRow>,
}

/// One fiduciary role and who the plan names to it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FiduciaryRow {
    /// The role in words, e.g. `Successor trustee`.
    pub role: String,
    /// Who serves first.
    pub first: String,
    /// Who serves after, in successor order.
    pub successors: Vec<String>,
}

/// One line of the estate's asset schedule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetRow {
    pub description: String,
    pub category: String,
    /// How it passes at death, in words.
    pub passes_by: String,
    /// `Funded`, `In progress`, `Not funded`, or `Consent needed`.
    pub funding: String,
    pub note: String,
}

/// The tone a status or step state reads in.
#[must_use]
pub fn tone(state: &str) -> BadgeTone {
    match state {
        "Executed" | "Done" | "Funded" | "Signed" | "Filed" | "Delivered" | "Agreed"
        | "Accepted" => BadgeTone::Ready,
        "At ceremony" | "In progress" | "Now" | "Sent" | "Viewed" | "Negotiating"
        | "Not funded" => BadgeTone::Review,
        "Blocked" | "Declined" | "Consent needed" | "Open" | "Open term" | "Hold" | "Voided"
        | "Expired" => BadgeTone::Blocked,
        "Approved" | "Next" => BadgeTone::Next,
        _ => BadgeTone::Default,
    }
}

/// The `instrument_set` section.
#[component]
pub fn InstrumentSetSection(rows: Vec<InstrumentCard>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "instrument_set",
            aria_labelledby: "section-instrument_set-title",
            Panel { id: "section-instrument_set".to_string(), title: "Instruments and execution".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No estate instrument is open on this matter yet.".to_string() }
                    }
                } else {
                    ul { class: "matter-cards",
                        for card in rows.iter() {
                            li { class: "matter-cards__card", "data-status": "{card.status}",
                                div { class: "matter-cards__head",
                                    strong { "{card.title}" }
                                    Badge { tone: tone(&card.status), "{card.status}" }
                                }
                                if !card.kind.is_empty() {
                                    p { class: "matter-cards__meta", "{card.kind}" }
                                }
                                for blocker in card.blockers.iter() {
                                    p { class: "matter-cards__blocker",
                                        "Blocked by {blocker.kind.to_lowercase()}: "
                                        if blocker.href.is_empty() {
                                            span { "{blocker.text}" }
                                        } else {
                                            a { class: "nav-link", href: "{blocker.href}", "{blocker.text}" }
                                        }
                                    }
                                }
                                if card.steps.is_empty() {
                                    p { class: "matter-cards__meta", "Its workflow declares no signing formality." }
                                } else {
                                    ol { class: "matter-steps",
                                        for step in card.steps.iter() {
                                            li { class: "matter-steps__step", "data-state": "{step.state}",
                                                span { "{step.label}" }
                                                " "
                                                Badge { tone: tone(&step.state), "{step.state}" }
                                                if !step.on.is_empty() {
                                                    span { class: "matter-cards__meta", " {step.on}" }
                                                }
                                            }
                                        }
                                    }
                                }
                                if !card.href.is_empty() {
                                    a { class: "nav-link", href: "{card.href}", "Read the outline" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `signing_ceremony` section.
#[component]
pub fn SigningCeremonySection(view: CeremonyView) -> Element {
    let date = if view.date.is_empty() {
        "Not scheduled yet".to_string()
    } else {
        view.date.clone()
    };
    let place = if view.place.is_empty() {
        "Not recorded yet".to_string()
    } else {
        view.place.clone()
    };
    rsx! {
        section {
            class: "matter-section",
            "data-section": "signing_ceremony",
            aria_labelledby: "section-signing_ceremony-title",
            Panel { id: "section-signing_ceremony".to_string(), title: "Signing ceremony".to_string(),
                if view.instruments.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No instrument on this matter declares a signing formality yet.".to_string() }
                    }
                } else {
                    dl { class: "matter-caption__facts",
                        dt { "Date" }
                        dd { "{date}" }
                        dt { "Place" }
                        dd { "{place}" }
                    }
                    for instrument in view.instruments.iter() {
                        h3 { class: "matter-section__subhead", "{instrument.title}" }
                        ol { class: "matter-steps",
                            for step in instrument.steps.iter() {
                                li { class: "matter-steps__step", "data-state": "{step.state}",
                                    span { "{step.label}" }
                                    " "
                                    Badge { tone: tone(&step.state), "{step.state}" }
                                    if !step.on.is_empty() {
                                        span { class: "matter-cards__meta", " {step.on}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// A stacked bar of shares, read as one image whose label lists every share,
/// beside the table that carries the same numbers.
#[component]
pub fn ShareBar(label: String, segments: Vec<(String, u32)>) -> Element {
    rsx! {
        div { class: "matter-share-bar", role: "img", aria_label: "{label}",
            for (index, (name, basis_points)) in segments.iter().enumerate() {
                span {
                    class: "matter-share-bar__segment matter-share--{index % 6}",
                    title: "{name}",
                    style: "width: {f64::from(*basis_points) / 100.0}%;",
                }
            }
        }
    }
}

/// The `distribution_plan` section.
#[component]
pub fn DistributionPlanSection(view: DistributionView) -> Element {
    let segments: Vec<(String, u32)> = view
        .shares
        .iter()
        .filter_map(|share| share.basis_points.map(|bp| (share.beneficiary.clone(), bp)))
        .collect();
    let label = format!(
        "Residuary shares: {}",
        view.shares
            .iter()
            .map(|share| format!("{} {}", share.beneficiary, share.share))
            .collect::<Vec<_>>()
            .join(", ")
    );
    rsx! {
        section {
            class: "matter-section",
            "data-section": "distribution_plan",
            aria_labelledby: "section-distribution_plan-title",
            Panel {
                id: "section-distribution_plan".to_string(),
                title: "Distribution at death".to_string(),
                note: (!view.source.is_empty()).then(|| format!("Residue under the {}", view.source)),
                if view.shares.is_empty() {
                    div { class: "matter-section__empty",
                        Empty {
                            title: "No residuary shares are recorded yet.".to_string(),
                            description: "The shares appear here once the trust's questionnaire records its residuary beneficiaries.".to_string(),
                        }
                    }
                } else {
                    if !segments.is_empty() {
                        ShareBar { label, segments: segments.clone() }
                    }
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Beneficiary" }
                                    th { scope: "col", "Share" }
                                }
                            }
                            tbody {
                                for (index, share) in view.shares.iter().enumerate() {
                                    tr {
                                        td {
                                            if share.basis_points.is_some() {
                                                span { class: "matter-share-swatch matter-share--{index % 6}", aria_hidden: "true" }
                                            }
                                            "{share.beneficiary}"
                                        }
                                        td { "{share.share}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `administration_steps` section.
#[component]
pub fn AdministrationStepsSection(view: StagesView) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "administration_steps",
            aria_labelledby: "section-administration_steps-title",
            Panel {
                id: "section-administration_steps".to_string(),
                title: "Stages".to_string(),
                note: (!view.notation.is_empty()).then(|| view.notation.clone()),
                if view.stages.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No workflow is running on this matter yet.".to_string() }
                    }
                } else {
                    ol { class: "matter-stages",
                        for stage in view.stages.iter() {
                            li { class: "matter-stages__stage", "data-state": "{stage.state}",
                                Badge { tone: tone(&stage.state), "{stage.state}" }
                                strong { "{stage.label}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `fiduciary_roster` section.
#[component]
pub fn FiduciaryRosterSection(rows: Vec<FiduciaryRow>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "fiduciary_roster",
            aria_labelledby: "section-fiduciary_roster-title",
            Panel { id: "section-fiduciary_roster".to_string(), title: "Fiduciaries and successors".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No fiduciary is named on this matter yet.".to_string() }
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Role" }
                                    th { scope: "col", "First" }
                                    th { scope: "col", "Successors" }
                                }
                            }
                            tbody {
                                for row in rows.iter() {
                                    tr {
                                        td { "{row.role}" }
                                        td { "{row.first}" }
                                        td {
                                            if row.successors.is_empty() {
                                                span { class: "matter-cards__meta", "None named" }
                                            } else {
                                                {row.successors.join(", then ")}
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `asset_schedule` section.
#[component]
pub fn AssetScheduleSection(rows: Vec<AssetRow>) -> Element {
    let funded = rows.iter().filter(|row| row.funding == "Funded").count();
    let note = (!rows.is_empty()).then(|| format!("{funded} of {} funded", rows.len()));
    rsx! {
        section {
            class: "matter-section",
            "data-section": "asset_schedule",
            aria_labelledby: "section-asset_schedule-title",
            Panel { id: "section-asset_schedule".to_string(), title: "Asset schedule and funding".to_string(), note,
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No estate asset is scheduled on this matter yet.".to_string() }
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Asset" }
                                    th { scope: "col", "Category" }
                                    th { scope: "col", "How it passes" }
                                    th { scope: "col", "Funding" }
                                    th { scope: "col", "Note" }
                                }
                            }
                            tbody {
                                for row in rows.iter() {
                                    tr { "data-funding": "{row.funding}",
                                        td { "{row.description}" }
                                        td { "{row.category}" }
                                        td { "{row.passes_by}" }
                                        td { Badge { tone: tone(&row.funding), "{row.funding}" } }
                                        td { "{row.note}" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Build the estate sections from the matter's Notations.
#[cfg(feature = "server")]
pub mod derive {
    use super::{
        AssetRow, BlockerView, CeremonyInstrument, CeremonyView, DistributionView, FiduciaryRow,
        InstrumentCard, ShareView, StageView, StagesView, StepView,
    };
    use crate::sections::instruments::{
        formalities, outline_href, stage_label, standing, words, Formality, Instrument, Standing,
    };

    /// The Draft kinds an estate plan executes.
    const ESTATE_KINDS: &[&str] = &["trust", "will", "directive"];

    fn is_estate(instrument: &Instrument) -> bool {
        ESTATE_KINDS.contains(&instrument.kind.as_str())
    }

    fn step(formality: &Formality) -> StepView {
        StepView {
            label: formality.label.clone(),
            state: formality.state.label().to_string(),
            on: formality.on.clone(),
        }
    }

    /// One card per estate instrument. Open terms name the question that
    /// answers them; the lawyer lens also reads each hold's words, and the
    /// client lens learns only that a drafting point is open.
    #[must_use]
    pub fn instrument_cards(
        instruments: &[Instrument],
        code: &str,
        lawyer: bool,
    ) -> Vec<InstrumentCard> {
        instruments
            .iter()
            .filter(|i| is_estate(i))
            .map(|instrument| {
                let status = standing(instrument);
                let mut blockers = Vec::new();
                if status == Standing::Blocked {
                    blockers.extend(instrument.open_terms.iter().map(|term| BlockerView {
                        kind: "Open term".to_string(),
                        text: term.label.clone(),
                        href: outline_href(code, instrument.id, &term.fragment),
                    }));
                    if lawyer {
                        blockers.extend(instrument.holds.iter().map(|hold| BlockerView {
                            kind: "Hold".to_string(),
                            text: hold.text.clone(),
                            href: outline_href(code, instrument.id, &hold.fragment),
                        }));
                    } else if !instrument.holds.is_empty() {
                        blockers.push(BlockerView {
                            kind: "Hold".to_string(),
                            text: "a drafting point your lawyer is resolving".to_string(),
                            href: String::new(),
                        });
                    }
                }
                InstrumentCard {
                    title: instrument.title.clone(),
                    kind: words(&instrument.kind),
                    status: status.label().to_string(),
                    steps: formalities(instrument).iter().map(step).collect(),
                    blockers,
                    href: outline_href(code, instrument.id, ""),
                }
            })
            .collect()
    }

    /// Whether an answer's question code records the ceremony's `part`
    /// (`date`, or `place` / `location`).
    fn ceremony_key(code: &str, parts: &[&str]) -> bool {
        let code = code.to_ascii_lowercase();
        ["ceremony", "signing", "execution"]
            .iter()
            .any(|word| code.contains(word))
            && parts.iter().any(|part| code.contains(part))
    }

    /// The signing ceremony: its date and place from the estate Notations'
    /// answers (a question whose code names the ceremony, signing, or
    /// execution date or place), else the date of an open Deadline this lens
    /// sees whose authority names a signing ceremony; and each estate
    /// instrument's formalities.
    #[must_use]
    pub fn ceremony(
        instruments: &[Instrument],
        deadlines: &[crate::sections::DeadlineCard],
    ) -> CeremonyView {
        let estate: Vec<&Instrument> = instruments.iter().filter(|i| is_estate(i)).collect();
        let answer = |parts: &[&str]| {
            estate
                .iter()
                .find_map(|i| i.answer(|code| ceremony_key(code, parts)))
                .map(store::answers::display_value)
                .unwrap_or_default()
        };
        let date = Some(answer(&["date"]))
            .filter(|date| !date.is_empty())
            .or_else(|| {
                deadlines
                    .iter()
                    .find(|row| {
                        let authority = row.authority.to_ascii_lowercase();
                        authority.contains("ceremony") || authority.contains("signing")
                    })
                    .map(|row| row.due.clone())
            })
            .unwrap_or_default();
        CeremonyView {
            date,
            place: answer(&["place", "location"]),
            instruments: estate
                .iter()
                .filter_map(|instrument| {
                    let steps: Vec<StepView> = formalities(instrument).iter().map(step).collect();
                    (!steps.is_empty()).then(|| CeremonyInstrument {
                        title: instrument.title.clone(),
                        steps,
                    })
                })
                .collect(),
        }
    }

    /// The first value under any of `keys` in a JSON object, as text.
    fn field(object: &serde_json::Map<String, serde_json::Value>, keys: &[&str]) -> String {
        keys.iter()
            .find_map(|key| object.get(*key))
            .map(|value| match value {
                serde_json::Value::Object(inner) => field(inner, &["name", "value"]),
                serde_json::Value::String(text) => text.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    }

    /// A share as hundredths of a percent, when it reads as a number.
    fn basis_points(share: &str) -> Option<u32> {
        let number: f64 = share.trim().trim_end_matches('%').trim().parse().ok()?;
        (0.0..=100.0)
            .contains(&number)
            .then(|| (number * 100.0).round())
            .map(|bp| {
                // `bp` is in [0, 10000] here, so the cast cannot truncate.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let bp = bp as u32;
                bp
            })
    }

    fn share_row(beneficiary: &str, share: &str) -> ShareView {
        let share = share.trim().to_string();
        let basis_points = basis_points(&share);
        let share = if basis_points.is_some() && !share.ends_with('%') {
            format!("{share}%")
        } else {
            share
        };
        ShareView {
            beneficiary: beneficiary.trim().to_string(),
            share,
            basis_points,
        }
    }

    /// The shares one residuary answer records: an array of records, each
    /// with a name and a share, or a line such as `A Example 50%; B Example
    /// 50%`.
    fn shares(value: &serde_json::Value) -> Vec<ShareView> {
        const NAME: &[&str] = &["name", "beneficiary", "person", "entity", "label"];
        const SHARE: &[&str] = &["share", "percent", "percentage", "fraction"];
        match value {
            serde_json::Value::Array(rows) => rows
                .iter()
                .filter_map(|row| row.as_object())
                .map(|row| share_row(&field(row, NAME), &field(row, SHARE)))
                .filter(|row| !row.beneficiary.is_empty())
                .collect(),
            other => store::answers::display_value(other)
                .split([';', '\n'])
                .filter(|part| !part.trim().is_empty())
                .map(|part| match part.trim().rsplit_once(char::is_whitespace) {
                    Some((name, share)) if basis_points(share).is_some() => {
                        share_row(name.trim_end_matches([':', ',']), share)
                    }
                    _ => share_row(part, ""),
                })
                .collect(),
        }
    }

    /// The residuary shares the trust Notation's answers record: the latest
    /// answer to a question whose code names the residue. A trust with no
    /// such answer, or a matter with no trust, is the empty state.
    #[must_use]
    pub fn distribution(instruments: &[Instrument]) -> DistributionView {
        instruments
            .iter()
            .filter(|i| i.kind == "trust")
            .find_map(|trust| {
                let value = trust.answer(|code| code.to_ascii_lowercase().contains("residu"))?;
                let shares = shares(value);
                (!shares.is_empty()).then(|| DistributionView {
                    source: trust.title.clone(),
                    shares,
                })
            })
            .unwrap_or_default()
    }

    /// The stages of the Notation the matter is waiting on: the one furthest
    /// along its workflow and not yet closed, else the newest. Each stage is
    /// done, the one it sits at now, the next, or later.
    #[must_use]
    pub fn stages(instruments: &[Instrument]) -> StagesView {
        let waiting =
            store::notations::furthest_along_state(instruments.iter().map(|i| i.state.as_str()));
        let Some(instrument) = waiting
            .and_then(|state| instruments.iter().find(|i| i.state == state))
            .or_else(|| instruments.last())
            .filter(|i| !i.workflow.is_empty())
        else {
            return StagesView::default();
        };
        let current = workflows::StateName::from(instrument.state.as_str());
        let at = instrument.workflow.iter().position(|state| {
            workflows::StateName::from(state.as_str()).prefix() == current.prefix()
        });
        let mut stages: Vec<StageView> = Vec::new();
        for (index, state) in instrument.workflow.iter().enumerate() {
            let label = stage_label(state);
            if stages.last().is_some_and(|last| last.label == label) {
                continue;
            }
            let state = if instrument.closed() || at.is_some_and(|at| index < at) {
                "Done"
            } else if at == Some(index) {
                "Now"
            } else if stages.last().is_some_and(|last| last.state == "Now")
                || (at.is_none() && stages.is_empty())
            {
                "Next"
            } else {
                "Later"
            };
            stages.push(StageView {
                label,
                state: state.to_string(),
            });
        }
        StagesView {
            notation: instrument.title.clone(),
            stages,
        }
    }

    /// The roster: one row per role, in the store's role order, with the
    /// first named and each successor after. `name` resolves a Party.
    #[must_use]
    pub fn fiduciaries(
        roles: &[store::fiduciary_roles::FiduciaryRole],
        name: impl Fn(&store::project_references::Party) -> String,
    ) -> Vec<FiduciaryRow> {
        let mut rows = Vec::new();
        for role in store::fiduciary_roles::ROLES {
            let mut named: Vec<&store::fiduciary_roles::FiduciaryRole> =
                roles.iter().filter(|r| r.role == role).collect();
            if named.is_empty() {
                continue;
            }
            named.sort_by_key(|r| r.successor_order);
            let mut in_order = named.iter().map(|r| name(&r.fiduciary));
            rows.push(FiduciaryRow {
                role: if role == "agent_poa" {
                    "Agent under power of attorney".to_string()
                } else {
                    words(role)
                },
                first: in_order.next().unwrap_or_default(),
                successors: in_order.collect(),
            });
        }
        rows
    }

    /// The asset schedule, in the store's order, each line's codes in words.
    #[must_use]
    pub fn assets(rows: &[store::estate_assets::EstateAsset]) -> Vec<AssetRow> {
        rows.iter()
            .map(|asset| AssetRow {
                description: asset.description.clone(),
                category: words(&asset.category),
                passes_by: words(&asset.passes_by),
                funding: words(&asset.funding_state),
                note: asset.note.clone().unwrap_or_default(),
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::{assets, ceremony, distribution, fiduciaries, instrument_cards, stages};
        use crate::sections::instruments::tests::will;
        use crate::sections::instruments::{Hold, Instrument, OpenTerm};

        fn trust(answer: serde_json::Value) -> Instrument {
            Instrument {
                title: "Revocable living trust".to_string(),
                kind: "trust".to_string(),
                answers: vec![("residuary_beneficiaries".to_string(), answer)],
                ..will("generate_pdf")
            }
        }

        #[test]
        fn a_blocked_card_names_its_open_term_and_hides_hold_words_from_a_client() {
            let mut blocked = will("generate_pdf");
            blocked.open_terms.push(OpenTerm {
                question: "person__successor_agent".into(),
                label: "Who is the successor agent?".into(),
                fragment: "section-ii-p1".into(),
            });
            blocked.holds.push(Hold {
                text: "Confirm the gifting power.".into(),
                fragment: "section-iii-p2".into(),
            });
            let onboarding = Instrument {
                kind: "onboarding".into(),
                ..will("END")
            };
            let lawyer = instrument_cards(&[blocked.clone(), onboarding], "sample-estate", true);
            assert_eq!(lawyer.len(), 1, "only estate kinds get a card");
            assert_eq!(lawyer[0].status, "Blocked");
            assert_eq!(lawyer[0].blockers[0].text, "Who is the successor agent?");
            assert!(lawyer[0].blockers[0]
                .href
                .ends_with("/outline#section-ii-p1"));
            assert_eq!(lawyer[0].blockers[1].text, "Confirm the gifting power.");
            let client = instrument_cards(&[blocked], "sample-estate", false);
            assert!(!format!("{client:?}").contains("gifting power"));
            assert_eq!(client[0].blockers.len(), 2);
        }

        #[test]
        fn the_ceremony_reads_its_date_and_place_from_answers_or_a_deadline() {
            let mut instrument = will("client_signature");
            instrument.answers = vec![
                (
                    "signing_date".into(),
                    store::answers::primitive("2026-11-04"),
                ),
                (
                    "ceremony_location".into(),
                    store::answers::primitive("Firm office"),
                ),
            ];
            let view = ceremony(&[instrument], &[]);
            assert_eq!(view.date, "2026-11-04");
            assert_eq!(view.place, "Firm office");
            assert_eq!(view.instruments[0].steps.len(), 3);
            let deadline = crate::sections::DeadlineCard {
                due: "2026-11-05 14:00 -08:00".into(),
                authority: "Signing ceremony".into(),
                ..Default::default()
            };
            let fallback = ceremony(&[will("client_signature")], &[deadline]);
            assert_eq!(fallback.date, "2026-11-05 14:00 -08:00");
            assert!(fallback.place.is_empty());
        }

        #[test]
        fn residuary_shares_read_from_records_or_a_line_and_are_empty_otherwise() {
            let records = distribution(&[trust(serde_json::json!([
                { "name": "A Example", "share": 50 },
                { "beneficiary": { "name": "B Example" }, "percent": "30%" },
                { "name": "C Example", "share": "20" }
            ]))]);
            assert_eq!(records.source, "Revocable living trust");
            let shares: Vec<(&str, &str, Option<u32>)> = records
                .shares
                .iter()
                .map(|s| (s.beneficiary.as_str(), s.share.as_str(), s.basis_points))
                .collect();
            assert_eq!(
                shares,
                [
                    ("A Example", "50%", Some(5000)),
                    ("B Example", "30%", Some(3000)),
                    ("C Example", "20%", Some(2000))
                ]
            );
            let line = distribution(&[trust(store::answers::primitive(
                "A Example 50%; B Example 50%",
            ))]);
            assert_eq!(line.shares.len(), 2);
            assert_eq!(line.shares[1].beneficiary, "B Example");
            let none = distribution(&[will("END")]);
            assert!(none.shares.is_empty());
        }

        #[test]
        fn the_roster_orders_successors_and_assets_read_in_words() {
            use store::fiduciary_roles::FiduciaryRole;
            use store::project_references::Party;
            let role = |role: &str, order: i64, n: u128| FiduciaryRole {
                id: uuid::Uuid::now_v7(),
                project_id: uuid::Uuid::nil(),
                role: role.into(),
                fiduciary: Party::person(uuid::Uuid::from_u128(n)),
                successor_order: order,
                notation_id: None,
                inserted_at: String::new(),
                updated_at: String::new(),
            };
            let name = |party: &Party| format!("Person {}", party.id.as_u128());
            let rows = fiduciaries(
                &[
                    role("trustee", 2, 2),
                    role("agent_poa", 1, 3),
                    role("trustee", 1, 1),
                ],
                name,
            );
            assert_eq!(rows[0].role, "Trustee");
            assert_eq!(rows[0].first, "Person 1");
            assert_eq!(rows[0].successors, ["Person 2"]);
            assert_eq!(rows[1].role, "Agent under power of attorney");
            assert!(fiduciaries(&[], name).is_empty());
            let schedule = assets(&[store::estate_assets::EstateAsset {
                id: uuid::Uuid::now_v7(),
                project_id: uuid::Uuid::nil(),
                category: "real_property".into(),
                description: "Synthetic residence".into(),
                passes_by: "beneficiary_designation".into(),
                funding_state: "consent_needed".into(),
                note: None,
                inserted_at: String::new(),
                updated_at: String::new(),
            }]);
            assert_eq!(schedule[0].category, "Real property");
            assert_eq!(schedule[0].passes_by, "Beneficiary designation");
            assert_eq!(schedule[0].funding, "Consent needed");
        }

        #[test]
        fn stages_follow_the_notation_the_matter_waits_on() {
            let view = stages(&[will("END"), will("witnesses")]);
            let states: Vec<(&str, &str)> = view
                .stages
                .iter()
                .map(|s| (s.label.as_str(), s.state.as_str()))
                .collect();
            assert_eq!(
                states,
                [
                    ("Lawyer review", "Done"),
                    ("Document prepared", "Done"),
                    ("Client signs", "Done"),
                    ("Witnesses", "Now"),
                    ("Notarization", "Next")
                ]
            );
            assert!(stages(&[]).stages.is_empty());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AssetRow, BlockerView, CeremonyInstrument, CeremonyView, DistributionView, FiduciaryRow,
        InstrumentCard, ShareView, StageView, StagesView, StepView,
    };
    use crate::sections::tests::render;
    use crate::sections::SectionView;

    fn step(label: &str, state: &str) -> StepView {
        StepView {
            label: label.into(),
            state: state.into(),
            on: String::new(),
        }
    }

    fn card(status: &str) -> InstrumentCard {
        InstrumentCard {
            title: format!("Synthetic {status} instrument"),
            kind: "Will".into(),
            status: status.into(),
            steps: vec![
                step("Testator signs", "Done"),
                step("Witnesses sign", "Not started"),
            ],
            blockers: Vec::new(),
            href: "/app/projects/sample-estate/n/outline".into(),
        }
    }

    #[test]
    fn every_instrument_status_renders_on_its_card() {
        for status in ["Drafting", "Approved", "At ceremony", "Executed"] {
            let html = render(SectionView::InstrumentSet(vec![card(status)]));
            assert!(
                html.contains(&format!("data-status=\"{status}\"")),
                "{html}"
            );
            assert!(html.contains("Witnesses sign"), "{html}");
            assert!(html.contains("Read the outline"), "{html}");
            assert!(!html.contains("matter-section__empty"), "{html}");
        }
    }

    #[test]
    fn a_blocked_card_names_its_open_term_with_a_link() {
        let mut blocked = card("Blocked");
        blocked.blockers.push(BlockerView {
            kind: "Open term".into(),
            text: "Successor agent".into(),
            href: "/app/projects/sample-estate/n/outline#section-ii-p1".into(),
        });
        let html = render(SectionView::InstrumentSet(vec![blocked]));
        assert!(html.contains("Blocked by open term"), "{html}");
        assert!(html.contains("Successor agent"), "{html}");
        assert!(html.contains("outline#section-ii-p1"), "{html}");
    }

    #[test]
    fn the_ceremony_says_what_is_not_recorded() {
        let html = render(SectionView::SigningCeremony(CeremonyView {
            date: String::new(),
            place: "Firm office".into(),
            instruments: vec![CeremonyInstrument {
                title: "Pour-over will".into(),
                steps: vec![step("Notarized", "In progress")],
            }],
        }));
        assert!(html.contains("Not scheduled yet"), "{html}");
        assert!(html.contains("Firm office"), "{html}");
        assert!(html.contains("Pour-over will"), "{html}");
    }

    #[test]
    fn the_distribution_draws_a_labelled_bar_beside_its_table() {
        let html = render(SectionView::DistributionPlan(DistributionView {
            source: "Revocable living trust".into(),
            shares: vec![
                ShareView {
                    beneficiary: "A Example".into(),
                    share: "60%".into(),
                    basis_points: Some(6000),
                },
                ShareView {
                    beneficiary: "B Example".into(),
                    share: "40%".into(),
                    basis_points: Some(4000),
                },
            ],
        }));
        assert!(html.contains("role=\"img\""), "{html}");
        assert!(
            html.contains("aria-label=\"Residuary shares: A Example 60%, B Example 40%\""),
            "{html}"
        );
        assert!(html.contains("width: 60%;"), "{html}");
        assert!(html.contains("<table"), "{html}");
    }

    #[test]
    fn the_roster_lists_successors_in_order() {
        let html = render(SectionView::FiduciaryRoster(vec![FiduciaryRow {
            role: "Trustee".into(),
            first: "A Example".into(),
            successors: vec!["B Example".into(), "C Example".into()],
        }]));
        assert!(html.contains("B Example, then C Example"), "{html}");
        assert!(!html.contains("matter-section__empty"), "{html}");
    }

    #[test]
    fn every_funding_state_renders_and_the_note_counts_funded() {
        let states = ["Funded", "In progress", "Not funded", "Consent needed"];
        let rows: Vec<AssetRow> = states
            .iter()
            .map(|funding| AssetRow {
                description: format!("Synthetic {funding} asset"),
                category: "Account".into(),
                passes_by: "Trust".into(),
                funding: (*funding).into(),
                note: String::new(),
            })
            .collect();
        let html = render(SectionView::AssetSchedule(rows));
        for funding in states {
            assert!(
                html.contains(&format!("data-funding=\"{funding}\"")),
                "{html}"
            );
        }
        assert!(html.contains("1 of 4 funded"), "{html}");
    }

    #[test]
    fn stages_render_their_states() {
        let html = render(SectionView::AdministrationSteps(StagesView {
            notation: "Pour-over will".into(),
            stages: vec![
                StageView {
                    label: "Lawyer review".into(),
                    state: "Done".into(),
                },
                StageView {
                    label: "Witnesses".into(),
                    state: "Now".into(),
                },
            ],
        }));
        assert!(html.contains("data-state=\"Now\""), "{html}");
        assert!(html.contains("Pour-over will"), "{html}");
    }
}
