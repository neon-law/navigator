//! The matter page's Deadlines panel — open Project Deadlines, soonest first,
//! with completed and cancelled rows in a collapsed past section.
//!
//! The rows are a view of `project_deadlines`; nothing here derives a date.
//! The firm lens sees every open row. The client lens sees only a row that
//! carries a client lead time, because an unset client lead means the client
//! is never warned about that date — the panel follows the same rule rather
//! than inventing a visibility flag of its own.

use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

/// One deadline as the panel renders it. Every field is display-ready, so
/// the wasm client build never needs the `store` row or a date library.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadlineRow {
    pub id: String,
    /// What is due; empty for a row recorded without a title.
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub visibility: String,
    /// The stored due instant with weekday and its original offset.
    pub due: String,
    /// Whose date it is, in plain words.
    pub role: String,
    /// What kind of date it is, in plain words; empty on a row recorded
    /// before categories existed.
    pub category: String,
    /// The full citation, left empty when the row has no citation.
    pub authority: String,
    /// Short text shown before the citation is expanded.
    pub authority_preview: String,
    /// Why the date binds, as a chip label.
    pub authority_kind: String,
    /// Stored status: `open`, `complete`, or `cancelled`.
    pub status: String,
    /// `Overdue`, `Due soon`, `Upcoming`, `Complete`, or `Cancelled`.
    pub state: String,
    pub overdue: bool,
}

/// The panel heading.
pub const HEADING: &str = "Deadlines";

/// What the panel says when no open deadline is visible to this viewer.
pub const EMPTY_MESSAGE: &str = "No open deadlines on this project.";

/// Open and historical rows after applying the viewer's visibility lens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeadlineGroups {
    pub open: Vec<DeadlineRow>,
    pub past: Vec<DeadlineRow>,
}

/// The open deadlines `rows` a viewer may see, soonest first.
///
/// `client_lens` keeps only the rows with a client lead time, and measures
/// "due soon" against that lead; the firm measures it against the internal
/// lead. A complete or cancelled row is history, not a deadline to meet, so it
/// drops out. A row whose stored instant does not parse is still shown, never
/// silently hidden, and is never called overdue.
#[cfg(feature = "server")]
#[must_use]
pub fn deadline_rows(
    rows: &[store::project_deadlines::Deadline],
    now: chrono::DateTime<chrono::Utc>,
    client_lens: bool,
) -> Vec<DeadlineRow> {
    deadline_groups(rows, now, client_lens).open
}

/// The open and historical Project Deadlines visible through one lens.
///
/// Client visibility is applied before status grouping so neither open nor
/// historical internal rows can reach a client panel.
#[cfg(feature = "server")]
#[must_use]
pub fn deadline_groups(
    rows: &[store::project_deadlines::Deadline],
    now: chrono::DateTime<chrono::Utc>,
    client_lens: bool,
) -> DeadlineGroups {
    use chrono::DateTime;

    let mut visible: Vec<(Option<DateTime<chrono::FixedOffset>>, DeadlineRow)> = rows
        .iter()
        .filter(|row| !client_lens || row.client_lead_days.is_some())
        .map(|row| {
            let due_at = DateTime::parse_from_rfc3339(&row.due_at).ok();
            let lead_days = if client_lens {
                row.client_lead_days
            } else {
                row.internal_lead_days
            };
            let open = row.status == store::project_deadlines::STATUS_OPEN;
            let overdue = open && due_at.is_some_and(|due| due < now);
            let due_soon = open
                && !overdue
                && due_at
                    .zip(lead_days)
                    .is_some_and(|(due, days)| due - chrono::Duration::days(days) <= now);
            let state = if overdue {
                "Overdue"
            } else if due_soon {
                "Due soon"
            } else if row.status == store::project_deadlines::STATUS_COMPLETE {
                "Complete"
            } else if row.status == store::project_deadlines::STATUS_CANCELLED {
                "Cancelled"
            } else {
                "Upcoming"
            };
            let due = due_at.map_or_else(
                || row.due_at.clone(),
                |due| due.format("%A, %-d %b %y %H:%M %:z").to_string(),
            );
            let authority = row.authority.clone();
            let authority_kind = authority_kind_label(&row.authority_kind).to_string();
            let authority_preview = if authority.trim().is_empty() {
                authority_kind.clone()
            } else {
                let mut preview: String = authority.chars().take(120).collect();
                if authority.chars().count() > 120 {
                    preview.push('…');
                }
                preview
            };
            (
                due_at,
                DeadlineRow {
                    id: row.id.to_string(),
                    title: row.title.clone().unwrap_or_default(),
                    description: row.description.clone().unwrap_or_default(),
                    visibility: if row.client_lead_days.is_some() {
                        "Client"
                    } else {
                        "Internal"
                    }
                    .to_string(),
                    due,
                    role: role_label(&row.calendar_role).to_string(),
                    category: category_label(row.category.as_deref().unwrap_or_default())
                        .to_string(),
                    authority,
                    authority_preview,
                    authority_kind,
                    status: row.status.clone(),
                    state: state.to_string(),
                    overdue,
                },
            )
        })
        .collect();
    visible.sort_by(|(a, _), (b, _)| match (a, b) {
        (Some(a), Some(b)) => a.cmp(b),
        (None, Some(_)) => std::cmp::Ordering::Less,
        (Some(_), None) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    let (mut open, mut past) = (Vec::new(), Vec::new());
    for (_, row) in visible {
        if row.status == store::project_deadlines::STATUS_OPEN {
            open.push(row);
        } else {
            past.push(row);
        }
    }
    DeadlineGroups { open, past }
}

/// `calendar_role` in plain words.
#[must_use]
pub fn role_label(calendar_role: &str) -> &'static str {
    match calendar_role {
        "firm" => "Our deadline",
        "opponent" => "Other side's deadline",
        "hearing" => "Hearing",
        _ => "Deadline",
    }
}

/// `category` in plain words. A row recorded without a category — the
/// historical shape — carries none, and an unknown value names itself.
#[must_use]
pub fn category_label(category: &str) -> &'static str {
    match category {
        "court" => "Court",
        "hearing" => "Hearing",
        "discovery" => "Discovery",
        "internal" => "Internal",
        "" => "",
        _ => "Uncategorized",
    }
}

/// `authority_kind` in plain words, for a row recorded without a citation.
#[must_use]
pub fn authority_kind_label(authority_kind: &str) -> &'static str {
    match authority_kind {
        "statute" => "Statute",
        "court_rule" => "Court rule",
        "court_order" => "Court order",
        "contract" => "Contract",
        "internal" => "Internal target",
        _ => "Unstated authority",
    }
}

/// The panel: open deadlines with overdue rows marked and a collapsed history
/// of completed or cancelled deadlines.
#[component]
pub fn ProjectDeadlinesPanel(
    section_class: String,
    rows: Vec<DeadlineRow>,
    past: Vec<DeadlineRow>,
) -> Element {
    let overdue = rows.iter().filter(|row| row.overdue).count();
    rsx! {
        section { class: "{section_class} project-deadlines", id: "deadlines",
            h2 { "{HEADING}" }
            if overdue > 0 {
                p { class: "project-deadlines__alert", role: "alert",
                    if overdue == 1 {
                        "1 deadline is overdue."
                    } else {
                        "{overdue} deadlines are overdue."
                    }
                }
            }
            div { class: "nav-table-wrap",
                table { class: "nav-table",
                    thead {
                        tr {
                            th { scope: "col", "Due" }
                            th { scope: "col", "Deadline" }
                            th { scope: "col", "Authority" }
                            th { scope: "col", "Category" }
                            th { scope: "col", "Status" }
                        }
                    }
                    tbody {
                        if rows.is_empty() {
                            tr {
                                td { colspan: "5", class: "nav-muted", "{EMPTY_MESSAGE}" }
                            }
                        } else {
                            for row in rows.iter() {
                                tr {
                                    key: "{row.id}",
                                    class: if row.overdue { "project-deadlines__row project-deadlines__row--overdue" } else { "project-deadlines__row" },
                                    "data-deadline": "{row.id}",
                                    td { "{row.due}" }
                                    td {
                                        if !row.title.is_empty() {
                                            strong { class: "project-deadlines__title", "{row.title} " }
                                        }
                                        span { class: "project-deadlines__chip project-deadlines__chip--role status-chip", "{row.role}" }
                                        span { class: "status-chip", "{row.visibility}" }
                                        if !row.description.is_empty() { p { "{row.description}" } }
                                    }
                                    td {
                                        span { class: "project-deadlines__chip project-deadlines__chip--authority status-chip", "{row.authority_kind}" }
                                        if !row.authority.is_empty() {
                                            details { class: "project-deadlines__authority",
                                                summary { "{row.authority_preview}" }
                                                p { "{row.authority}" }
                                            }
                                        }
                                    }
                                    td {
                                        if !row.category.is_empty() {
                                            span { class: "project-deadlines__chip project-deadlines__chip--category status-chip", "{row.category}" }
                                        }
                                    }
                                    td {
                                        span {
                                            class: if row.overdue { "status-chip status-chip--due project-deadlines__state--overdue" } else if row.state == "Due soon" { "status-chip status-chip--due" } else { "status-chip" },
                                            "{row.state}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if !past.is_empty() {
                details { class: "project-deadlines__past",
                    summary { "Past ({past.len()})" }
                    div { class: "nav-table-wrap",
                        table { class: "nav-table",
                            thead { tr {
                                th { scope: "col", "Due" }
                                th { scope: "col", "Deadline" }
                                th { scope: "col", "Authority" }
                                th { scope: "col", "Category" }
                                th { scope: "col", "Status" }
                            } }
                            tbody {
                                for row in past.iter() {
                                    tr {
                                        key: "past-{row.id}",
                                        "data-deadline": "{row.id}",
                                        td { "{row.due}" }
                                        td {
                                            if !row.title.is_empty() {
                                                strong { class: "project-deadlines__title", "{row.title} " }
                                            }
                                            span { class: "project-deadlines__chip project-deadlines__chip--role status-chip", "{row.role}" }
                                        span { class: "status-chip", "{row.visibility}" }
                                        if !row.description.is_empty() { p { "{row.description}" } }
                                        }
                                        td {
                                            span { class: "project-deadlines__chip project-deadlines__chip--authority status-chip", "{row.authority_kind}" }
                                            if !row.authority.is_empty() {
                                                details { class: "project-deadlines__authority",
                                                    summary { "{row.authority_preview}" }
                                                    p { "{row.authority}" }
                                                }
                                            }
                                        }
                                        td {
                                            if !row.category.is_empty() {
                                                span { class: "project-deadlines__chip project-deadlines__chip--category status-chip", "{row.category}" }
                                            }
                                        }
                                        td { "{row.state}" }
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
    use super::*;

    fn panel(rows: Vec<DeadlineRow>) -> String {
        dioxus_ssr::render_element(rsx! {
            ProjectDeadlinesPanel {
                section_class: "lawyer-detail__section".to_string(),
                rows,
                past: Vec::new(),
            }
        })
    }

    fn row(id: &str, state: &str, overdue: bool) -> DeadlineRow {
        DeadlineRow {
            id: id.to_string(),
            title: String::new(),
            description: "Synthetic description".to_string(),
            visibility: "Internal".to_string(),
            due: "2026-11-02 17:00 -08:00".to_string(),
            role: "Our deadline".to_string(),
            category: "Court".to_string(),
            authority: "Order setting briefing schedule".to_string(),
            authority_preview: "Order setting briefing schedule".to_string(),
            authority_kind: "Court order".to_string(),
            status: "open".to_string(),
            state: state.to_string(),
            overdue,
        }
    }

    fn panel_with_past(rows: Vec<DeadlineRow>, past: Vec<DeadlineRow>) -> String {
        dioxus_ssr::render_element(rsx! {
            ProjectDeadlinesPanel {
                section_class: "lawyer-detail__section".to_string(),
                rows,
                past,
            }
        })
    }

    #[test]
    fn the_panel_shows_a_title_only_when_the_row_has_one() {
        let mut titled = row("a", "Upcoming", false);
        titled.title = "Answer to the complaint".to_string();
        let html = panel(vec![titled, row("b", "Upcoming", false)]);
        assert_eq!(
            html.matches("project-deadlines__title").count(),
            1,
            "{html}"
        );
        assert!(html.contains("Answer to the complaint"), "{html}");
    }

    #[test]
    fn panel_renders_a_category_chip_only_when_the_row_has_one() {
        let mut categorized = row("a", "Upcoming", false);
        categorized.category = "Discovery".to_string();
        let html = panel(vec![categorized]);
        assert!(html.contains("project-deadlines__chip--category"), "{html}");
        assert!(html.contains(">Discovery<"), "{html}");
        let mut uncategorized = row("b", "Upcoming", false);
        uncategorized.category = String::new();
        let html = panel(vec![uncategorized]);
        assert!(
            !html.contains("project-deadlines__chip--category"),
            "{html}"
        );
    }

    #[test]
    fn category_labels_cover_every_closed_vocabulary_value() {
        for (category, label) in [
            ("court", "Court"),
            ("hearing", "Hearing"),
            ("discovery", "Discovery"),
            ("internal", "Internal"),
        ] {
            assert_eq!(category_label(category), label);
        }
        assert_eq!(category_label(""), "");
        assert_eq!(category_label("filing"), "Uncategorized");
    }

    #[test]
    fn an_empty_panel_says_none_are_open() {
        let html = panel(Vec::new());
        assert!(html.contains(HEADING), "{html}");
        assert!(html.contains(EMPTY_MESSAGE), "{html}");
        assert!(html.contains("colspan=\"4\""), "{html}");
        assert!(!html.contains("role=\"alert\""), "{html}");
    }

    #[test]
    fn an_overdue_row_is_marked_and_announced() {
        let html = panel(vec![row("a", "Overdue", true), row("b", "Upcoming", false)]);
        assert!(html.contains("project-deadlines__row--overdue"), "{html}");
        assert!(html.contains("1 deadline is overdue."), "{html}");
        assert_eq!(html.matches("data-deadline=").count(), 2, "{html}");
        assert!(html.contains("Order setting briefing schedule"), "{html}");
    }

    #[test]
    fn two_overdue_rows_are_counted() {
        let html = panel(vec![row("a", "Overdue", true), row("b", "Overdue", true)]);
        assert!(html.contains("2 deadlines are overdue."), "{html}");
    }

    #[test]
    fn panel_renders_role_and_authority_chips_with_expandable_authority() {
        let html = panel_with_past(vec![row("a", "Upcoming", false)], Vec::new());

        assert!(html.contains("project-deadlines__chip--role"), "{html}");
        assert!(
            html.contains("project-deadlines__chip--authority"),
            "{html}"
        );
        assert!(
            html.contains("<details class=\"project-deadlines__authority\">"),
            "{html}"
        );
        assert!(html.contains("Order setting briefing schedule"), "{html}");
    }

    #[test]
    fn panel_collapses_completed_and_cancelled_rows_in_the_past_section() {
        let mut complete = row("complete", "Complete", false);
        complete.status = "complete".to_string();
        let mut cancelled = row("cancelled", "Cancelled", false);
        cancelled.status = "cancelled".to_string();
        let html = panel_with_past(Vec::new(), vec![complete, cancelled]);

        assert!(
            html.contains("<details class=\"project-deadlines__past\">"),
            "{html}"
        );
        assert!(html.contains("Past (2)"), "{html}");
        assert!(html.contains("data-deadline=\"complete\""), "{html}");
        assert!(html.contains("data-deadline=\"cancelled\""), "{html}");
        assert!(!html.contains("project-deadlines__past\" open"), "{html}");
    }

    #[test]
    fn labels_cover_every_closed_vocabulary_value() {
        for (role, label) in [
            ("firm", "Our deadline"),
            ("opponent", "Other side's deadline"),
            ("hearing", "Hearing"),
        ] {
            assert_eq!(role_label(role), label);
        }
        for kind in [
            "statute",
            "court_rule",
            "court_order",
            "contract",
            "internal",
        ] {
            assert_ne!(authority_kind_label(kind), "Unstated authority", "{kind}");
        }
    }

    #[cfg(feature = "server")]
    mod rows {
        use super::super::{deadline_groups, deadline_rows};
        use store::project_deadlines::Deadline;
        use uuid::Uuid;

        fn deadline(due_at: &str, status: &str, client_lead: Option<i64>) -> Deadline {
            Deadline {
                id: Uuid::new_v4(),
                project_id: Uuid::nil(),
                title: None,
                description: None,
                due_at: due_at.to_string(),
                authority_kind: "court_order".to_string(),
                authority: String::new(),
                source: "lawyer".to_string(),
                source_document_id: None,
                owner_person_id: None,
                status: status.to_string(),
                calendar_role: "firm".to_string(),
                category: Some("court".to_string()),
                internal_lead_days: Some(7),
                client_lead_days: client_lead,
                replay_key: None,
                inserted_at: String::new(),
                updated_at: String::new(),
                calendar_sequence: None,
            }
        }

        fn now() -> chrono::DateTime<chrono::Utc> {
            "2026-10-20T12:00:00Z".parse().unwrap()
        }

        #[test]
        fn open_rows_sort_soonest_first_with_overdue_and_due_soon_marked() {
            let rows = deadline_rows(
                &[
                    deadline("2026-12-01T17:00:00-08:00", "open", None),
                    deadline("2026-10-01T17:00:00-07:00", "open", None),
                    deadline("2026-10-24T17:00:00-07:00", "open", None),
                    deadline("2026-09-01T17:00:00-07:00", "complete", None),
                    deadline("2026-09-02T17:00:00-07:00", "cancelled", None),
                ],
                now(),
                false,
            );
            let states: Vec<_> = rows.iter().map(|r| r.state.as_str()).collect();
            assert_eq!(states, ["Overdue", "Due soon", "Upcoming"]);
            assert!(rows[0].overdue);
            assert_eq!(rows[0].due, "Thursday, 1 Oct 26 17:00 -07:00");
            assert_eq!(rows[0].authority_kind, "Court order");
        }

        #[test]
        fn the_client_lens_shows_only_rows_with_a_client_lead() {
            let rows = deadline_rows(
                &[
                    deadline("2026-10-24T17:00:00-07:00", "open", None),
                    deadline("2026-10-24T17:00:00-07:00", "open", Some(3)),
                ],
                now(),
                true,
            );
            assert_eq!(rows.len(), 1);
            // Four days out with a three-day client lead: not yet due soon.
            assert_eq!(rows[0].state, "Upcoming");
        }

        #[test]
        fn client_lens_filters_internal_rows_before_open_and_past_grouping() {
            let groups = deadline_groups(
                &[
                    deadline("2026-10-24T17:00:00-07:00", "open", None),
                    deadline("2026-09-01T17:00:00-07:00", "complete", None),
                    deadline("2026-10-24T17:00:00-07:00", "open", Some(3)),
                    deadline("2026-09-02T17:00:00-07:00", "cancelled", Some(3)),
                ],
                now(),
                true,
            );

            assert_eq!(groups.open.len(), 1);
            assert_eq!(groups.past.len(), 1);
            assert_eq!(groups.open[0].status, "open");
            assert_eq!(groups.past[0].status, "cancelled");
        }

        #[test]
        fn completed_and_cancelled_rows_are_past_and_never_overdue() {
            let groups = deadline_groups(
                &[
                    deadline("2026-09-01T17:00:00-07:00", "complete", None),
                    deadline("2026-09-02T17:00:00-07:00", "cancelled", None),
                ],
                now(),
                false,
            );

            assert!(groups.open.is_empty());
            assert_eq!(
                groups
                    .past
                    .iter()
                    .map(|row| row.state.as_str())
                    .collect::<Vec<_>>(),
                ["Complete", "Cancelled"]
            );
            assert!(groups.past.iter().all(|row| !row.overdue));
        }

        #[test]
        fn long_authorities_have_a_bounded_preview_and_keep_the_full_text() {
            let citation = "A".repeat(140);
            let mut row = deadline("2026-12-01T17:00:00-08:00", "open", None);
            row.authority = citation.clone();
            let groups = deadline_groups(&[row], now(), false);

            assert_eq!(groups.open[0].authority_preview.chars().count(), 121);
            assert!(groups.open[0].authority_preview.ends_with('…'));
            assert_eq!(groups.open[0].authority, citation);
        }

        #[test]
        fn a_title_reaches_the_row_and_a_missing_one_is_empty() {
            let mut titled = deadline("2026-12-01T17:00:00-08:00", "open", None);
            titled.title = Some("Answer to the complaint".to_string());
            let plain = deadline("2026-12-02T17:00:00-08:00", "open", None);
            let rows = deadline_rows(&[titled, plain], now(), false);
            assert_eq!(rows[0].title, "Answer to the complaint");
            assert_eq!(rows[1].title, "");
        }

        #[test]
        fn an_unparseable_instant_is_shown_and_never_overdue() {
            let rows = deadline_rows(&[deadline("not a date", "open", None)], now(), false);
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].due, "not a date");
            assert!(!rows[0].overdue);
        }
    }
}
