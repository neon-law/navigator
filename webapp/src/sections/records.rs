//! The litigation board's record sections: `deadline_board`,
//! `party_roster`, and `efiling_receipts`.
//!
//! Every value here is display-ready and wasm-safe; [`super::derive`]
//! builds them from Deadlines, participation, Service, Project References,
//! and e-filing transactions, already scoped to the lens.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

use super::SectionRow;
use crate::components::{Badge, BadgeTone, Empty, FactCard, FactGrid, Panel};

/// One open Deadline on the board.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadlineCard {
    /// What is due; empty for a row recorded without a title.
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub visibility: String,
    /// The stored due instant in its own offset.
    pub due: String,
    /// `Hard` when a statute, court rule, court order, or contract binds the
    /// date; `Soft` for an internal target or an unstated authority.
    pub binding: String,
    /// Why the date binds: the citation, or the authority kind when none.
    pub authority: String,
    /// Whose date it is, in plain words.
    pub role: String,
    /// `Overdue`, `Due soon`, or `Upcoming`, against this lens's lead time.
    pub state: String,
    pub overdue: bool,
}

/// One group of the party roster: a heading over its rows.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterGroup {
    pub heading: String,
    pub rows: Vec<SectionRow>,
}

/// The `party_roster` section's data. No groups is the empty state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RosterView {
    pub groups: Vec<RosterGroup>,
}

/// One court e-filing submission.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptRow {
    /// `YYYY-MM-DD`: the filed date when the court accepted it, else the date
    /// the submission was recorded.
    pub submitted: String,
    pub title: String,
    /// The e-filing service provider's transaction number.
    pub transaction: String,
    pub status: String,
    /// The firm's note on the submission; empty on the client lens.
    pub note: String,
}

fn state_tone(state: &str) -> BadgeTone {
    match state {
        "Overdue" | "Rejected" => BadgeTone::Blocked,
        "Due soon" | "Unknown" => BadgeTone::Review,
        "Accepted" => BadgeTone::Ready,
        _ => BadgeTone::Next,
    }
}

/// The `deadline_board` section.
#[component]
pub fn DeadlineBoardSection(rows: Vec<DeadlineCard>) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "deadline_board",
            aria_labelledby: "section-deadline_board-title",
            Panel { id: "section-deadline_board".to_string(), title: "Deadlines".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No open deadlines on this matter.".to_string() }
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Deadline" }
                                    th { scope: "col", "Description" }
                                    th { scope: "col", "Visibility" }
                                    th { scope: "col", "Due" }
                                    th { scope: "col", "Binding" }
                                    th { scope: "col", "Authority" }
                                    th { scope: "col", "Whose" }
                                    th { scope: "col", "State" }
                                }
                            }
                            tbody {
                                for row in rows.iter() {
                                    tr { "data-binding": "{row.binding}",
                                        td {
                                            if row.title.is_empty() {
                                                span { class: "nav-muted", "Untitled" }
                                            } else {
                                                "{row.title}"
                                            }
                                        }
                                        td { "{row.description}" }
                                        td { "{row.visibility}" }
                                        td { "{row.due}" }
                                        td {
                                            Badge {
                                                tone: if row.binding == "Hard" { BadgeTone::Active } else { BadgeTone::Default },
                                                "{row.binding}"
                                            }
                                        }
                                        td { "{row.authority}" }
                                        td { "{row.role}" }
                                        td { Badge { tone: state_tone(&row.state), "{row.state}" } }
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

/// The `party_roster` section.
#[component]
pub fn PartyRosterSection(view: RosterView) -> Element {
    rsx! {
        section {
            class: "matter-section",
            "data-section": "party_roster",
            aria_labelledby: "section-party_roster-title",
            Panel { id: "section-party_roster".to_string(), title: "Parties and counsel".to_string(),
                if view.groups.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No parties are recorded on this matter yet.".to_string() }
                    }
                } else {
                    FactGrid {
                        for group in view.groups.iter() {
                            FactCard { title: group.heading.clone(),
                                ul { class: "matter-section__rows",
                                    for row in group.rows.iter() {
                                        li { class: "matter-section__row",
                                            strong { "{row.label}" }
                                            if !row.detail.is_empty() {
                                                " · "
                                                span { "{row.detail}" }
                                            }
                                            if !row.status.is_empty() {
                                                " "
                                                Badge { "{row.status}" }
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

/// The `efiling_receipts` section.
#[component]
pub fn EfilingReceiptsSection(rows: Vec<ReceiptRow>) -> Element {
    let notes = rows.iter().any(|row| !row.note.is_empty());
    rsx! {
        section {
            class: "matter-section",
            "data-section": "efiling_receipts",
            aria_labelledby: "section-efiling_receipts-title",
            Panel { id: "section-efiling_receipts".to_string(), title: "E-filing receipts".to_string(),
                if rows.is_empty() {
                    div { class: "matter-section__empty",
                        Empty { title: "No e-filing submissions are recorded yet.".to_string() }
                    }
                } else {
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead {
                                tr {
                                    th { scope: "col", "Submitted" }
                                    th { scope: "col", "Title" }
                                    th { scope: "col", "Transaction" }
                                    th { scope: "col", "Status" }
                                    if notes {
                                        th { scope: "col", "Note" }
                                    }
                                }
                            }
                            tbody {
                                for row in rows.iter() {
                                    tr {
                                        td { "{row.submitted}" }
                                        td { "{row.title}" }
                                        td { code { "{row.transaction}" } }
                                        td { Badge { tone: state_tone(&row.status), "{row.status}" } }
                                        if notes {
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
}

#[cfg(test)]
mod tests {
    use super::{DeadlineCard, ReceiptRow, RosterGroup, RosterView};
    use crate::sections::tests::render;
    use crate::sections::{SectionRow, SectionView};

    #[test]
    fn the_deadline_board_shows_binding_authority_and_state() {
        let html = render(SectionView::DeadlineBoard(vec![DeadlineCard {
            title: "Opposition to the motion".to_string(),
            description: "Synthetic description".to_string(),
            visibility: "Internal".to_string(),
            due: "2026-11-02 17:00 -08:00".to_string(),
            binding: "Hard".to_string(),
            authority: "Synthetic order setting briefing".to_string(),
            role: "Our deadline".to_string(),
            state: "Due soon".to_string(),
            overdue: false,
        }]));
        for text in [
            "Opposition to the motion",
            "Synthetic description",
            "Internal",
            "2026-11-02 17:00 -08:00",
            "data-binding=\"Hard\"",
            "Synthetic order setting briefing",
            "Our deadline",
            "Due soon",
        ] {
            assert!(html.contains(text), "{text}: {html}");
        }
        assert!(!html.contains("matter-section__empty"), "{html}");
    }

    #[test]
    fn the_roster_renders_each_group_under_its_heading() {
        let html = render(SectionView::PartyRoster(RosterView {
            groups: vec![RosterGroup {
                heading: "Parties".to_string(),
                rows: vec![SectionRow {
                    label: "Alpha Example".to_string(),
                    detail: "Client".to_string(),
                    status: String::new(),
                }],
            }],
        }));
        assert!(html.contains("Parties"), "{html}");
        assert!(html.contains("Alpha Example"), "{html}");
        assert!(!html.contains("matter-section__empty"), "{html}");
    }

    #[test]
    fn receipts_show_submitted_title_transaction_and_status() {
        let html = render(SectionView::EfilingReceipts(vec![ReceiptRow {
            submitted: "2026-03-04".to_string(),
            title: "di-12-motion.pdf".to_string(),
            transaction: "TX-0001".to_string(),
            status: "Accepted".to_string(),
            note: String::new(),
        }]));
        for text in ["2026-03-04", "di-12-motion.pdf", "TX-0001", "Accepted"] {
            assert!(html.contains(text), "{text}: {html}");
        }
        assert!(!html.contains(">Note<"), "no note column without notes");
    }
}
