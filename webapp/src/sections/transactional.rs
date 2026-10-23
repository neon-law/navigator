//! The closing room's sections: `closing_checklist`, `signature_tracker`,
//! `term_strip`, `open_terms`, `term_comparison`, `requirement_tracker`,
//! `redline_view`, and `cap_table`.
//!
//! Every value here is display-ready and wasm-safe. The server half
//! ([`derive`]) builds them from the matter's Notations, Signatures,
//! Filings, Revisions, and rows, already scoped to the lens: a hold's words
//! and the contract review are the firm's, a client reads only the published,
//! client-visible revisions, and a client reads the `cap_table` Module's rows
//! only while it is enabled.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::estate::{tone, ShareBar};
use crate::components::{Badge, Empty, Panel, Progress, StatusCell, StatusStrip};

/// Every transactional section's data for one reader, built once per page.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClosingViews {
    pub cap_table: Vec<HoldingRow>,
    pub checklist: Vec<ChecklistItem>,
    pub signatures: Vec<SignatureRow>,
    pub terms: Vec<TermCell>,
    pub open_terms: Vec<OpenTermRow>,
    pub comparison: Vec<ComparisonRow>,
    pub requirements: Vec<RequirementRow>,
    pub redlines: Vec<RevisionDocument>,
}

/// The two groups a closing checklist reads in.
pub const CONDITIONS: &str = "Conditions precedent";
/// See [`CONDITIONS`].
pub const DELIVERABLES: &str = "Deliverables";

/// One line of the closing checklist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    /// [`CONDITIONS`] or [`DELIVERABLES`].
    pub group: String,
    pub label: String,
    pub detail: String,
    pub status: String,
    pub done: bool,
    /// Where to resolve it; empty for none.
    pub href: String,
}

/// One signature request.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignatureRow {
    pub signatory: String,
    pub instrument: String,
    /// `Sent`, `Signed`, `Declined`, `Voided`, or `Expired`.
    pub status: String,
    pub detail: String,
}

/// One term a Draft carries.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermCell {
    pub label: String,
    /// The answer that fills it; empty while it is open.
    pub value: String,
    /// `Agreed` or `Open`.
    pub status: String,
}

/// One open term or hold still to resolve.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenTermRow {
    /// `Open term` or `Hold`.
    pub kind: String,
    pub text: String,
    pub instrument: String,
    pub href: String,
}

/// One clause of the other side's paper against the client's position.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonRow {
    /// Where in the contract, e.g. `§ 7.2 Liability`.
    pub clause: String,
    pub instrument: String,
    /// How the proposal departs from the client's position.
    pub proposal: String,
    /// The redline the firm proposes; empty for none.
    pub position: String,
    pub severity: String,
    /// `Accepted` or `Open`.
    pub status: String,
}

/// One filing a workflow requires.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequirementRow {
    pub requirement: String,
    pub instrument: String,
    /// `Filed`, `In progress`, or `Not started`.
    pub status: String,
    pub detail: String,
}

/// One revision of a living document.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionRow {
    pub version: u32,
    pub label: String,
    /// `YYYY-MM-DD`.
    pub date: String,
    pub published: bool,
    /// The revision this reader treats as current.
    pub current: bool,
}

/// A living document with more than one revision this reader sees.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevisionDocument {
    pub document: String,
    /// Oldest first.
    pub revisions: Vec<RevisionRow>,
}

/// One holder's shares of one class.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldingRow {
    pub holder: String,
    pub class: String,
    pub shares: i64,
    /// The holding's share of every share recorded, in hundredths of a
    /// percent.
    pub basis_points: u32,
    pub note: String,
}

/// `12345` as `12,345`.
#[must_use]
pub fn grouped(number: i64) -> String {
    let digits = number.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    if number < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// Hundredths of a percent as `62.50%`.
#[must_use]
pub fn percent(basis_points: u32) -> String {
    format!("{}.{:02}%", basis_points / 100, basis_points % 100)
}

/// The `cap_table` section: an accessible bar beside the table that carries
/// the same numbers.
#[component]
pub fn CapTableSection(rows: Vec<HoldingRow>) -> Element {
    let segments: Vec<(String, u32)> = rows
        .iter()
        .map(|row| (format!("{} ({})", row.holder, row.class), row.basis_points))
        .collect();
    let label = format!(
        "Fully diluted ownership: {}",
        rows.iter()
            .map(|row| format!("{} {} {}", row.holder, row.class, percent(row.basis_points)))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let total: i64 = rows.iter().map(|row| row.shares).sum();
    rsx! {
        section {
            class: "matter-section",
            "data-section": "cap_table",
            aria_labelledby: "section-cap_table-title",
            Panel {
                id: "section-cap_table".to_string(),
                title: "Cap table".to_string(),
                note: (!rows.is_empty()).then(|| format!("Fully diluted · {} shares", grouped(total))),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No holding is recorded on this matter yet.".to_string() }
                    }
                } else {
                    ShareBar { label, segments }
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Holder" }
                                    th { scope: "col", "Class" }
                                    th { scope: "col", class: "matter-table__num", "Shares" }
                                    th { scope: "col", class: "matter-table__num", "Fully diluted" }
                                }
                            }
                            tbody {
                                for (index, row) in rows.iter().enumerate() {
                                    tr {
                                        td {
                                            span { class: "matter-share-swatch matter-share--{index % 6}", aria_hidden: "true" }
                                            "{row.holder}"
                                            if !row.note.is_empty() {
                                                span { class: "matter-cards__meta", " · {row.note}" }
                                            }
                                        }
                                        td { "{row.class}" }
                                        td { class: "matter-table__num", "{grouped(row.shares)}" }
                                        td { class: "matter-table__num", "{percent(row.basis_points)}" }
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

/// The `closing_checklist` section.
#[component]
pub fn ClosingChecklistSection(rows: Vec<ChecklistItem>) -> Element {
    let done = rows.iter().filter(|row| row.done).count();
    rsx! {
        section {
            class: "matter-section",
            "data-section": "closing_checklist",
            aria_labelledby: "section-closing_checklist-title",
            Panel {
                id: "section-closing_checklist".to_string(),
                title: "Closing checklist".to_string(),
                note: (!rows.is_empty()).then(|| format!("{done} of {} complete", rows.len())),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "Nothing is on the closing checklist yet.".to_string() }
                    }
                } else {
                    Progress { label: "Closing checklist".to_string(), value: Some(done), max: rows.len() }
                    for group in [CONDITIONS, DELIVERABLES] {
                        if rows.iter().any(|row| row.group == group) {
                            h3 { class: "matter-section__subhead", "{group}" }
                            ul { class: "matter-section__rows",
                                for row in rows.iter().filter(|row| row.group == group) {
                                    li { class: "matter-section__row", "data-done": "{row.done}",
                                        if row.href.is_empty() {
                                            strong { "{row.label}" }
                                        } else {
                                            a { class: "nav-link", href: "{row.href}", strong { "{row.label}" } }
                                        }
                                        if !row.detail.is_empty() {
                                            " · "
                                            span { class: "matter-cards__meta", "{row.detail}" }
                                        }
                                        " "
                                        Badge { tone: tone(&row.status), "{row.status}" }
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

/// The `signature_tracker` section.
#[component]
pub fn SignatureTrackerSection(rows: Vec<SignatureRow>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "signature_tracker",
            aria_labelledby: "section-signature_tracker-title",
            Panel { id: "section-signature_tracker".to_string(), title: "Signatures".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No document has been sent for signature yet.".to_string() }
                    }
                } else {
                    ul { class: "matter-section__rows",
                        for row in rows.iter() {
                            li { class: "matter-section__row", "data-status": "{row.status}",
                                strong { "{row.signatory}" }
                                " · {row.instrument}"
                                if !row.detail.is_empty() {
                                    " · "
                                    span { class: "matter-cards__meta", "{row.detail}" }
                                }
                                " "
                                Badge { tone: tone(&row.status), "{row.status}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `term_strip` section.
#[component]
pub fn TermStripSection(rows: Vec<TermCell>) -> Element {
    let cells: Vec<StatusCell> = rows
        .iter()
        .map(|row| {
            let value = if row.value.is_empty() {
                row.status.clone()
            } else {
                format!("{} · {}", row.value, row.status)
            };
            StatusCell::new(row.label.clone(), value).tone(tone(&row.status))
        })
        .collect();
    rsx! {
        section {
            class: "matter-section",
            "data-section": "term_strip",
            aria_labelledby: "section-term_strip-title",
            Panel { id: "section-term_strip".to_string(), title: "Terms".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No open document carries a term yet.".to_string() }
                    }
                } else {
                    StatusStrip { cells }
                }
            }
        }
    }
}

/// The `open_terms` section.
#[component]
pub fn OpenTermsSection(rows: Vec<OpenTermRow>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "open_terms",
            aria_labelledby: "section-open_terms-title",
            Panel { id: "section-open_terms".to_string(), title: "Open terms".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No term is open.".to_string() }
                    }
                } else {
                    ul { class: "matter-section__rows",
                        for row in rows.iter() {
                            li { class: "matter-section__row", "data-kind": "{row.kind}",
                                Badge { tone: tone(&row.kind), "{row.kind}" }
                                " "
                                a { class: "nav-link", href: "{row.href}", strong { "{row.text}" } }
                                " · "
                                span { class: "matter-cards__meta", "{row.instrument}" }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The `term_comparison` section.
#[component]
pub fn TermComparisonSection(rows: Vec<ComparisonRow>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "term_comparison",
            aria_labelledby: "section-term_comparison-title",
            Panel { id: "section-term_comparison".to_string(), title: "Proposal and position".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No contract review has findings yet.".to_string() }
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Clause" }
                                    th { scope: "col", "Their proposal" }
                                    th { scope: "col", "Our position" }
                                    th { scope: "col", "Severity" }
                                    th { scope: "col", "Status" }
                                }
                            }
                            tbody {
                                for row in rows.iter() {
                                    tr {
                                        td { cite { "{row.clause}" } br {} span { class: "matter-cards__meta", "{row.instrument}" } }
                                        td { "{row.proposal}" }
                                        td { "{row.position}" }
                                        td { "{row.severity}" }
                                        td { Badge { tone: tone(&row.status), "{row.status}" } }
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

/// The `requirement_tracker` section.
#[component]
pub fn RequirementTrackerSection(rows: Vec<RequirementRow>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "requirement_tracker",
            aria_labelledby: "section-requirement_tracker-title",
            Panel { id: "section-requirement_tracker".to_string(), title: "Filing requirements".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No workflow on this matter requires a filing.".to_string() }
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Requirement" }
                                    th { scope: "col", "Document" }
                                    th { scope: "col", "Status" }
                                    th { scope: "col", "Detail" }
                                }
                            }
                            tbody {
                                for row in rows.iter() {
                                    tr { "data-status": "{row.status}",
                                        td { "{row.requirement}" }
                                        td { "{row.instrument}" }
                                        td { Badge { tone: tone(&row.status), "{row.status}" } }
                                        td { "{row.detail}" }
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

/// The `redline_view` section: each living document's revisions, oldest
/// first, with the one this reader treats as current marked.
#[component]
pub fn RedlineViewSection(rows: Vec<RevisionDocument>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "redline_view",
            aria_labelledby: "section-redline_view-title",
            Panel { id: "section-redline_view".to_string(), title: "Revisions".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty {
                            title: "No document has a second revision yet.".to_string(),
                            description: "Each revision of a living document appears here once it is filed.".to_string(),
                        }
                    }
                } else {
                    for document in rows.iter() {
                        h3 { class: "matter-section__subhead", "{document.document}" }
                        ol { class: "matter-section__rows",
                            for revision in document.revisions.iter() {
                                li { class: "matter-section__row", "data-current": "{revision.current}",
                                    strong { "v{revision.version}" }
                                    " · {revision.label}"
                                    if !revision.date.is_empty() {
                                        " · "
                                        span { class: "matter-cards__meta", "{revision.date}" }
                                    }
                                    if revision.current {
                                        " "
                                        Badge { tone: crate::components::BadgeTone::Active, "Current" }
                                    } else if !revision.published {
                                        " "
                                        Badge { "Unpublished" }
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

/// Build the transactional sections from the matter's rows.
#[cfg(feature = "server")]
pub mod derive {
    use super::{
        ChecklistItem, ClosingViews, ComparisonRow, HoldingRow, OpenTermRow, RequirementRow,
        RevisionDocument, RevisionRow, SignatureRow, TermCell, CONDITIONS, DELIVERABLES,
    };
    use crate::sections::instruments::{
        is_submission, outline_href, stage_label, standing, words, Instrument, Standing,
    };

    /// Everything the transactional sections read, already scoped to the lens.
    pub struct Inputs<'a> {
        pub instruments: &'a [Instrument],
        pub code: &'a str,
        pub lawyer: bool,
        pub holdings: &'a [store::cap_table_holdings::CapTableHolding],
        pub name: &'a dyn Fn(&store::project_references::Party) -> String,
        pub assets: &'a [store::assets::Asset],
    }

    /// Every transactional section's data.
    #[must_use]
    pub fn views(inputs: &Inputs<'_>) -> ClosingViews {
        let deal: Vec<&Instrument> = inputs
            .instruments
            .iter()
            .filter(|i| !matches!(i.kind.as_str(), "onboarding" | "offboarding"))
            .collect();
        ClosingViews {
            cap_table: cap_table(inputs.holdings, inputs.name),
            checklist: checklist(&deal, inputs.code, inputs.lawyer),
            signatures: signatures(&deal),
            terms: terms(&deal),
            open_terms: open_terms(&deal, inputs.code, inputs.lawyer),
            comparison: comparison(&deal),
            requirements: requirements(&deal),
            redlines: redlines(inputs.assets, inputs.lawyer),
        }
    }

    /// The closing checklist. The conditions precedent are what still holds
    /// a document back: each open term, and each hold (on the client lens,
    /// one line saying a drafting point is open). The deliverables are the
    /// documents themselves, each with where it stands and what comes next.
    #[must_use]
    pub fn checklist(deal: &[&Instrument], code: &str, lawyer: bool) -> Vec<ChecklistItem> {
        let mut out = Vec::new();
        for instrument in deal.iter().filter(|i| !i.closed()) {
            out.extend(instrument.open_terms.iter().map(|term| ChecklistItem {
                group: CONDITIONS.to_string(),
                label: format!("Answer: {}", term.label),
                detail: instrument.title.clone(),
                status: "Open term".to_string(),
                done: false,
                href: outline_href(code, instrument.id, &term.fragment),
            }));
            if lawyer {
                out.extend(instrument.holds.iter().map(|hold| ChecklistItem {
                    group: CONDITIONS.to_string(),
                    label: format!("Resolve: {}", hold.text),
                    detail: instrument.title.clone(),
                    status: "Hold".to_string(),
                    done: false,
                    href: outline_href(code, instrument.id, &hold.fragment),
                }));
            } else if !instrument.holds.is_empty() {
                out.push(ChecklistItem {
                    group: CONDITIONS.to_string(),
                    label: "A drafting point your lawyer is resolving".to_string(),
                    detail: instrument.title.clone(),
                    status: "Hold".to_string(),
                    done: false,
                    href: String::new(),
                });
            }
        }
        for instrument in deal {
            let signed = instrument
                .signatures
                .iter()
                .any(|s| s.state == store::signatures::SignatureState::Completed);
            let (status, done) = match standing(instrument) {
                _ if instrument.closed() => ("Delivered", true),
                _ if signed => ("Signed", true),
                Standing::Executed => ("Signed", true),
                Standing::Blocked => ("Open term", false),
                Standing::AtCeremony => ("Awaiting signature", false),
                Standing::Approved => ("Approved", false),
                Standing::Drafting => ("Drafting", false),
            };
            let next = next_stage(instrument);
            out.push(ChecklistItem {
                group: DELIVERABLES.to_string(),
                label: instrument.title.clone(),
                detail: if done || next.is_empty() {
                    String::new()
                } else {
                    format!("Next: {next}")
                },
                status: status.to_string(),
                done,
                href: outline_href(code, instrument.id, ""),
            });
        }
        out
    }

    /// The stage after the one the Notation sits at, in words.
    fn next_stage(instrument: &Instrument) -> String {
        let current = workflows::StateName::from(instrument.state.as_str());
        let at = instrument
            .workflow
            .iter()
            .position(|s| workflows::StateName::from(s.as_str()).prefix() == current.prefix());
        let next = match at {
            Some(at) => instrument.workflow.get(at + 1),
            None if instrument.state == workflows::StateName::BEGIN => instrument.workflow.first(),
            None => None,
        };
        next.map(|s| stage_label(s)).unwrap_or_default()
    }

    /// One row per signature request, oldest first: the signer by name when
    /// the request names one, the document, and the request's state. The
    /// provider's record holds no viewed event, so none is shown.
    #[must_use]
    pub fn signatures(deal: &[&Instrument]) -> Vec<SignatureRow> {
        use store::signatures::SignatureState;

        let mut out = Vec::new();
        for instrument in deal {
            for (index, signature) in instrument.signatures.iter().enumerate() {
                let signatory = instrument
                    .signer_names
                    .get(index)
                    .filter(|name| !name.is_empty())
                    .cloned()
                    .or_else(|| signature.field.as_deref().map(words))
                    .unwrap_or_else(|| "Signer".to_string());
                let sent = format!("Sent {}", signature.inserted_at.format("%Y-%m-%d"));
                let (status, detail) = match signature.state {
                    SignatureState::Completed => (
                        "Signed",
                        signature.signed_at.as_deref().map_or(sent, |on| {
                            format!("Signed {}", on.chars().take(10).collect::<String>())
                        }),
                    ),
                    SignatureState::Requested if !signature.completed_signer_roles.is_empty() => (
                        "Sent",
                        format!(
                            "{sent} · signed so far: {}",
                            signature
                                .completed_signer_roles
                                .iter()
                                .map(|role| words(role))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    ),
                    SignatureState::Requested => ("Sent", sent),
                    SignatureState::Declined => ("Declined", sent),
                    SignatureState::Voided => ("Voided", sent),
                    SignatureState::Expired => ("Expired", sent),
                };
                out.push(SignatureRow {
                    signatory,
                    instrument: instrument.title.clone(),
                    status: status.to_string(),
                    detail,
                });
            }
        }
        out
    }

    /// Each term an open document carries, in Draft order: agreed, with the
    /// answer that fills it, or open.
    #[must_use]
    pub fn terms(deal: &[&Instrument]) -> Vec<TermCell> {
        let mut out = Vec::new();
        for instrument in deal.iter().filter(|i| !i.closed()) {
            for term in &instrument.agreed_terms {
                out.push(TermCell {
                    label: term.label.clone(),
                    value: instrument
                        .term_answer(&term.question)
                        .map(store::answers::display_value)
                        .unwrap_or_default(),
                    status: "Agreed".to_string(),
                });
            }
            for term in &instrument.open_terms {
                out.push(TermCell {
                    label: term.label.clone(),
                    value: String::new(),
                    status: "Open".to_string(),
                });
            }
        }
        out
    }

    /// Each open term and, on the lawyer lens, each hold, linked to where it
    /// sits in its document's outline.
    #[must_use]
    pub fn open_terms(deal: &[&Instrument], code: &str, lawyer: bool) -> Vec<OpenTermRow> {
        let mut out = Vec::new();
        for instrument in deal.iter().filter(|i| !i.closed()) {
            out.extend(instrument.open_terms.iter().map(|term| OpenTermRow {
                kind: "Open term".to_string(),
                text: term.label.clone(),
                instrument: instrument.title.clone(),
                href: outline_href(code, instrument.id, &term.fragment),
            }));
            if lawyer {
                out.extend(instrument.holds.iter().map(|hold| OpenTermRow {
                    kind: "Hold".to_string(),
                    text: hold.text.clone(),
                    instrument: instrument.title.clone(),
                    href: outline_href(code, instrument.id, &hold.fragment),
                }));
            }
        }
        out
    }

    /// Each finding of each document's latest contract review: the clause it
    /// cites, how the proposal departs from the client's playbook, the
    /// redline the firm proposes, and whether the reviewing lawyer accepted
    /// it. Only a lawyer read loads a review.
    #[must_use]
    pub fn comparison(deal: &[&Instrument]) -> Vec<ComparisonRow> {
        deal.iter()
            .filter_map(|i| i.review.as_ref().map(|review| (i, review)))
            .flat_map(|(instrument, review)| {
                store::contract_reviews::findings_of(review)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|finding| ComparisonRow {
                        clause: finding.clause_ref,
                        instrument: instrument.title.clone(),
                        proposal: finding.deviation,
                        position: finding.suggested_redline.unwrap_or_default(),
                        severity: words(&finding.severity),
                        status: if finding.accepted { "Accepted" } else { "Open" }.to_string(),
                    })
            })
            .collect()
    }

    /// Each submission step a document's workflow declares (mail, certified
    /// mail, e-filing, filing), filed when a Filing of that kind is recorded,
    /// in progress while the document sits at that step.
    #[must_use]
    pub fn requirements(deal: &[&Instrument]) -> Vec<RequirementRow> {
        let mut out = Vec::new();
        for instrument in deal {
            let current = workflows::StateName::from(instrument.state.as_str());
            for state in instrument.workflow.iter().filter(|s| is_submission(s)) {
                let prefix = workflows::StateName::from(state.as_str())
                    .prefix()
                    .to_string();
                let filing = instrument
                    .filings
                    .iter()
                    .find(|f| prefix == f.kind || prefix.starts_with(&format!("{}_", f.kind)));
                let (status, detail) = match filing {
                    Some(filing) => (
                        "Filed",
                        [
                            filing.office.clone(),
                            filing.reference.clone().unwrap_or_default(),
                            filing.submitted_at.chars().take(10).collect(),
                        ]
                        .into_iter()
                        .filter(|part| !part.is_empty())
                        .collect::<Vec<_>>()
                        .join(" · "),
                    ),
                    None if current.prefix() == prefix => ("In progress", String::new()),
                    None => ("Not started", String::new()),
                };
                out.push(RequirementRow {
                    requirement: stage_label(state),
                    instrument: instrument.title.clone(),
                    status: status.to_string(),
                    detail,
                });
            }
        }
        out
    }

    /// Each living document with two or more revisions this reader sees,
    /// oldest revision first. The lawyer lens sees every revision and treats
    /// the latest as current; the client lens sees only published,
    /// client-visible revisions and treats the latest of those as current.
    #[must_use]
    pub fn redlines(assets: &[store::assets::Asset], lawyer: bool) -> Vec<RevisionDocument> {
        let mut slugs: Vec<&str> = Vec::new();
        for asset in assets {
            if let Some(slug) = asset.slug.as_deref() {
                if !slugs.contains(&slug) {
                    slugs.push(slug);
                }
            }
        }
        slugs.sort_unstable();
        slugs
            .into_iter()
            .filter_map(|slug| {
                let mut chain: Vec<&store::assets::Asset> = assets
                    .iter()
                    .filter(|a| a.slug.as_deref() == Some(slug))
                    .filter(|a| {
                        lawyer
                            || (a.published_at.is_some()
                                && a.visibility == store::documents::visibility::CLIENT)
                    })
                    .collect();
                chain.sort_by_key(|a| a.id);
                if chain.len() < 2 {
                    return None;
                }
                let last = chain.len() - 1;
                Some(RevisionDocument {
                    document: chain[last]
                        .filename
                        .clone()
                        .unwrap_or_else(|| slug.to_string()),
                    revisions: chain
                        .iter()
                        .enumerate()
                        .map(|(index, asset)| RevisionRow {
                            version: u32::try_from(index + 1).unwrap_or(u32::MAX),
                            label: asset.filename.clone().unwrap_or_else(|| slug.to_string()),
                            date: asset
                                .published_at
                                .clone()
                                .or_else(|| asset.received_at.clone())
                                .map_or_else(
                                    || asset.inserted_at.format("%Y-%m-%d").to_string(),
                                    |on| on.chars().take(10).collect(),
                                ),
                            published: asset.published_at.is_some(),
                            current: index == last,
                        })
                        .collect(),
                })
            })
            .collect()
    }

    /// The cap table: each holding with its share of every share recorded,
    /// largest first within each class, as the store orders them. A row
    /// with no shares, or a table with none, reads as 0%.
    #[must_use]
    pub fn cap_table(
        holdings: &[store::cap_table_holdings::CapTableHolding],
        name: impl Fn(&store::project_references::Party) -> String,
    ) -> Vec<HoldingRow> {
        let total: i128 = holdings.iter().map(|h| i128::from(h.shares.max(0))).sum();
        holdings
            .iter()
            .map(|holding| {
                let basis_points = if total > 0 {
                    u32::try_from(i128::from(holding.shares.max(0)) * 10_000 / total)
                        .unwrap_or(10_000)
                } else {
                    0
                };
                HoldingRow {
                    holder: name(&holding.holder),
                    class: holding.share_class.clone(),
                    shares: holding.shares,
                    basis_points,
                    note: holding.vesting_note.clone().unwrap_or_default(),
                }
            })
            .collect()
    }

    #[cfg(test)]
    mod tests {
        use super::{
            cap_table, checklist, comparison, open_terms, redlines, requirements, signatures, terms,
        };
        use crate::sections::instruments::tests::will;
        use crate::sections::instruments::{Hold, Instrument, OpenTerm};
        use store::cap_table_holdings::CapTableHolding;
        use store::project_references::Party;

        fn holding(class: &str, shares: i64, n: u128) -> CapTableHolding {
            CapTableHolding {
                id: uuid::Uuid::now_v7(),
                project_id: uuid::Uuid::nil(),
                holder: Party::person(uuid::Uuid::from_u128(n)),
                share_class: class.into(),
                shares,
                vesting_note: None,
                inserted_at: String::new(),
                updated_at: String::new(),
            }
        }

        fn term(question: &str) -> OpenTerm {
            OpenTerm {
                question: question.into(),
                label: crate::sections::instruments::words(question),
                fragment: format!("section-i-{question}"),
            }
        }

        fn agreement(state: &str) -> Instrument {
            let mut instrument = will(state);
            instrument.title = "Synthetic stock purchase agreement".into();
            instrument.kind = "agreement".into();
            instrument
        }

        fn signature(state: store::signatures::SignatureState) -> store::signatures::Signature {
            store::signatures::Signature {
                id: uuid::Uuid::now_v7(),
                notation_id: uuid::Uuid::nil(),
                signer_person_id: None,
                field: Some("lead_investor".into()),
                provider: "docusign".into(),
                provider_id: "synthetic".into(),
                state,
                signed_at: Some("2026-10-09T12:00:00Z".into()),
                completed_signer_roles: Vec::new(),
                inserted_at: "2026-10-08T12:00:00Z".parse().unwrap(),
                updated_at: "2026-10-08T12:00:00Z".parse().unwrap(),
            }
        }

        #[test]
        fn the_checklist_lists_conditions_then_deliverables_and_hides_hold_words_from_a_client() {
            let mut blocked = agreement("generate_pdf");
            blocked.open_terms.push(term("pro_rata_threshold"));
            blocked.holds.push(Hold {
                text: "Confirm the synthetic option pool timing.".into(),
                fragment: "section-ii-p1".into(),
            });
            let closed = agreement("END");
            let lawyer = checklist(&[&blocked, &closed], "sample-transactional", true);
            let lines: Vec<(&str, &str, bool)> = lawyer
                .iter()
                .map(|i| (i.group.as_str(), i.status.as_str(), i.done))
                .collect();
            assert_eq!(
                lines,
                [
                    ("Conditions precedent", "Open term", false),
                    ("Conditions precedent", "Hold", false),
                    ("Deliverables", "Open term", false),
                    ("Deliverables", "Delivered", true),
                ]
            );
            assert!(lawyer[0].href.ends_with("#section-i-pro_rata_threshold"));
            let client = checklist(&[&blocked], "sample-transactional", false);
            assert!(!format!("{client:?}").contains("option pool timing"));
            assert_eq!(client[1].label, "A drafting point your lawyer is resolving");
        }

        #[test]
        fn each_signature_state_reads_in_words() {
            use store::signatures::SignatureState;
            let mut instrument = agreement("client_signature");
            let mut partial = signature(SignatureState::Requested);
            partial.completed_signer_roles = vec!["client".into()];
            instrument.signatures = vec![
                signature(SignatureState::Completed),
                partial,
                signature(SignatureState::Declined),
            ];
            instrument.signer_names =
                vec!["Synthetic Investor".into(), String::new(), String::new()];
            let rows = signatures(&[&instrument]);
            let states: Vec<&str> = rows.iter().map(|r| r.status.as_str()).collect();
            assert_eq!(states, ["Signed", "Sent", "Declined"]);
            assert_eq!(rows[0].signatory, "Synthetic Investor");
            assert_eq!(rows[0].detail, "Signed 2026-10-09");
            assert_eq!(rows[1].signatory, "Lead investor");
            assert!(rows[1].detail.contains("signed so far: Client"), "{rows:?}");
        }

        #[test]
        fn terms_read_agreed_with_their_answer_or_open() {
            let mut instrument = agreement("lawyer_review");
            instrument.agreed_terms.push(term("valuation"));
            instrument.open_terms.push(term("pro_rata_threshold"));
            instrument.answers = vec![(
                "custom_usd__valuation".into(),
                store::answers::primitive("$10,000,000"),
            )];
            let cells = terms(&[&instrument]);
            assert_eq!(cells[0].status, "Agreed");
            assert_eq!(cells[0].value, "$10,000,000");
            assert_eq!(cells[1].status, "Open");
            let lawyer = open_terms(&[&instrument], "m", true);
            assert_eq!(lawyer.len(), 1);
            assert!(terms(&[&agreement("END")]).is_empty());
        }

        #[test]
        fn findings_compare_proposal_and_position() {
            let mut instrument = agreement("lawyer_review");
            instrument.review = Some(store::contract_reviews::ContractReview {
                id: uuid::Uuid::now_v7(),
                notation_id: uuid::Uuid::nil(),
                playbook_id: uuid::Uuid::nil(),
                asset_id: None,
                status: "analyzed".into(),
                risk_summary: None,
                findings: serde_json::json!([{
                    "clause_ref": "§ 4.1 Pro rata",
                    "deviation": "Extends pro rata rights to every holder.",
                    "severity": "high",
                    "suggested_redline": "Limit pro rata rights to major investors.",
                    "accepted": false
                }]),
                inserted_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            });
            let rows = comparison(&[&instrument]);
            assert_eq!(rows[0].clause, "§ 4.1 Pro rata");
            assert_eq!(
                rows[0].position,
                "Limit pro rata rights to major investors."
            );
            assert_eq!(rows[0].status, "Open");
            assert!(comparison(&[&agreement("END")]).is_empty());
        }

        #[test]
        fn a_submission_step_is_filed_once_its_filing_is_recorded() {
            let mut instrument = agreement("mailroom_send");
            instrument.workflow = vec![
                "lawyer_review".into(),
                "mailroom_send".into(),
                "filing__secretary_of_state".into(),
            ];
            instrument.filings.push(store::filings::Filing {
                id: uuid::Uuid::now_v7(),
                notation_id: uuid::Uuid::nil(),
                kind: "filing".into(),
                office: "Synthetic Secretary of State".into(),
                reference: Some("SYN-1".into()),
                summary: String::new(),
                submitted_at: "2026-10-09T12:00:00Z".into(),
                inserted_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            });
            let rows = requirements(&[&instrument]);
            let states: Vec<&str> = rows.iter().map(|r| r.status.as_str()).collect();
            assert_eq!(states, ["In progress", "Filed"]);
            assert_eq!(
                rows[1].detail,
                "Synthetic Secretary of State · SYN-1 · 2026-10-09"
            );
        }

        #[test]
        fn a_client_sees_only_published_client_revisions() {
            let revision = |name: &str, published: bool, visibility: &str| store::assets::Asset {
                id: uuid::Uuid::now_v7(),
                storage_key: String::new(),
                secondary_storage_key: None,
                content_type: "application/pdf".into(),
                byte_size: 0,
                sha256_hex: String::new(),
                project_id: None,
                filename: Some(name.into()),
                kind: None,
                source: None,
                received_at: None,
                description: None,
                visibility: visibility.into(),
                slug: Some("spa".into()),
                published_at: published.then(|| "2026-10-01".to_string()),
                source_message_id: None,
                source_sender: None,
                source_received_at: None,
                source_subject: None,
                metadata: None,
                derived_from: None,
                transcript_quality: None,
                signing: None,
                notarization: Vec::new(),
                deleted_at: None,
                purge_after: None,
                deleted_by_person_id: None,
                deleted_commit_sha: None,
                legal_hold_at: None,
                purged_at: None,
                inserted_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            };
            let assets = vec![
                revision("spa-v1.docx", true, "client"),
                revision("spa-v2.docx", true, "client"),
                revision("spa-v3-redline.docx", false, "internal"),
            ];
            let lawyer = redlines(&assets, true);
            assert_eq!(lawyer[0].revisions.len(), 3);
            assert!(lawyer[0].revisions[2].current);
            let client = redlines(&assets, false);
            assert_eq!(client[0].revisions.len(), 2);
            assert_eq!(client[0].revisions[1].label, "spa-v2.docx");
            assert!(client[0].revisions[1].current);
            assert!(redlines(&assets[..1], true).is_empty());
        }

        #[test]
        fn each_holding_reads_its_fully_diluted_share() {
            let name = |party: &Party| format!("Holder {}", party.id.as_u128());
            let rows = cap_table(
                &[
                    holding("Common", 6_200_000, 1),
                    holding("Series Seed", 2_000_000, 2),
                    holding("Reserved", 1_800_000, 3),
                ],
                name,
            );
            let shares: Vec<u32> = rows.iter().map(|r| r.basis_points).collect();
            assert_eq!(shares, [6200, 2000, 1800]);
            assert_eq!(rows[0].holder, "Holder 1");
            assert!(cap_table(&[], name).is_empty());
            assert_eq!(
                cap_table(&[holding("Common", 0, 1)], name)[0].basis_points,
                0
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{grouped, percent, HoldingRow};
    use crate::sections::tests::render;
    use crate::sections::SectionView;

    #[test]
    fn numbers_read_grouped_and_as_percentages() {
        assert_eq!(grouped(6_200_000), "6,200,000");
        assert_eq!(grouped(999), "999");
        assert_eq!(percent(6250), "62.50%");
        assert_eq!(percent(5), "0.05%");
    }

    #[test]
    fn the_cap_table_draws_a_labelled_bar_and_a_table() {
        let html = render(SectionView::CapTable(vec![
            HoldingRow {
                holder: "Synthetic Founder".into(),
                class: "Common".into(),
                shares: 7_500,
                basis_points: 7500,
                note: String::new(),
            },
            HoldingRow {
                holder: "Synthetic Fund".into(),
                class: "Series Seed".into(),
                shares: 2_500,
                basis_points: 2500,
                note: "Four-year vesting".into(),
            },
        ]));
        assert!(html.contains("role=\"img\""), "{html}");
        assert!(
            html.contains("Fully diluted ownership: Synthetic Founder Common 75.00%"),
            "{html}"
        );
        assert!(html.contains("7,500"), "{html}");
        assert!(html.contains("25.00%"), "{html}");
        assert!(html.contains("Fully diluted · 10,000 shares"), "{html}");
        assert!(!html.contains("matter-section__empty"), "{html}");
    }
}

#[cfg(test)]
mod section_tests {
    use super::*;
    use crate::sections::tests::render;
    use crate::sections::SectionView;

    #[test]
    fn the_checklist_counts_done_lines_under_each_group() {
        let item = |group: &str, status: &str, done: bool| ChecklistItem {
            group: group.into(),
            label: format!("Synthetic {status}"),
            detail: String::new(),
            status: status.into(),
            done,
            href: String::new(),
        };
        let html = render(SectionView::ClosingChecklist(vec![
            item(CONDITIONS, "Open term", false),
            item(DELIVERABLES, "Signed", true),
        ]));
        assert!(html.contains("1 of 2 complete"), "{html}");
        assert!(html.contains(CONDITIONS), "{html}");
        assert!(html.contains(DELIVERABLES), "{html}");
        assert!(html.contains("role=\"progressbar\""), "{html}");
    }

    #[test]
    fn every_signature_status_renders() {
        for status in ["Sent", "Signed", "Declined"] {
            let html = render(SectionView::SignatureTracker(vec![SignatureRow {
                signatory: "Synthetic Investor".into(),
                instrument: "Synthetic agreement".into(),
                status: status.into(),
                detail: String::new(),
            }]));
            assert!(
                html.contains(&format!("data-status=\"{status}\"")),
                "{html}"
            );
        }
    }

    #[test]
    fn the_term_strip_says_agreed_or_open_in_words() {
        let html = render(SectionView::TermStrip(vec![
            TermCell {
                label: "Valuation".into(),
                value: "$10,000,000".into(),
                status: "Agreed".into(),
            },
            TermCell {
                label: "Pro rata threshold".into(),
                value: String::new(),
                status: "Open".into(),
            },
        ]));
        assert!(html.contains("$10,000,000 · Agreed"), "{html}");
        assert!(html.contains(">Open<"), "{html}");
    }

    #[test]
    fn an_open_term_links_to_its_place_in_the_outline() {
        let html = render(SectionView::OpenTerms(vec![OpenTermRow {
            kind: "Open term".into(),
            text: "Pro rata threshold".into(),
            instrument: "Synthetic agreement".into(),
            href: "/app/projects/sample-transactional/n/outline#section-iv-p1".into(),
        }]));
        assert!(html.contains("outline#section-iv-p1"), "{html}");
    }

    #[test]
    fn the_comparison_cites_its_clause() {
        let html = render(SectionView::TermComparison(vec![ComparisonRow {
            clause: "§ 4.1 Pro rata".into(),
            instrument: "Synthetic agreement".into(),
            proposal: "Every holder.".into(),
            position: "Major investors only.".into(),
            severity: "High".into(),
            status: "Open".into(),
        }]));
        assert!(html.contains("<cite>§ 4.1 Pro rata</cite>"), "{html}");
        assert!(html.contains("Major investors only."), "{html}");
    }

    #[test]
    fn every_requirement_status_renders() {
        for status in ["Filed", "In progress", "Not started"] {
            let html = render(SectionView::RequirementTracker(vec![RequirementRow {
                requirement: "Filing".into(),
                instrument: "Synthetic articles".into(),
                status: status.into(),
                detail: String::new(),
            }]));
            assert!(
                html.contains(&format!("data-status=\"{status}\"")),
                "{html}"
            );
        }
    }

    #[test]
    fn the_redline_view_marks_the_current_revision() {
        let html = render(SectionView::RedlineView(vec![RevisionDocument {
            document: "spa.docx".into(),
            revisions: vec![
                RevisionRow {
                    version: 1,
                    label: "spa-v1.docx".into(),
                    date: "2026-10-01".into(),
                    published: true,
                    current: false,
                },
                RevisionRow {
                    version: 2,
                    label: "spa-v2.docx".into(),
                    date: "2026-10-05".into(),
                    published: false,
                    current: true,
                },
            ],
        }]));
        assert!(html.contains("data-current=\"true\""), "{html}");
        assert!(html.contains(">Current<"), "{html}");
        assert!(html.contains("v2"), "{html}");
    }
}
