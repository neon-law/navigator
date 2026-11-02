//! The matter page's timeline rows: every [Docket
//! Entry](../../docs/glossary/docket-entry.md) on the matter's cases joined
//! with every open [Deadline](../../docs/glossary/deadline.md), in date
//! order, rendered through the matter calendar.
//!
//! The docket records what exists and the Deadlines module answers what is
//! due; this join is the one view that reads both side by side. Nothing here
//! derives a date: an entry's date is the one the court set (`scheduled_on`)
//! or the one it was filed on, and a deadline's is the stored due instant.

#[cfg(feature = "server")]
use crate::project_calendar::CalendarEvent;

/// The status cell of a docket entry row.
#[cfg(feature = "server")]
fn entry_status(entry: &store::cases::DocketEntry, superseded: bool) -> String {
    if superseded {
        return "Superseded".to_string();
    }
    match entry.kind.as_str() {
        "hearing" => "Hearing".to_string(),
        "trial" => "Trial".to_string(),
        _ if store::cases::is_source_pending(entry) => "Source pending".to_string(),
        _ => "Filed".to_string(),
    }
}

/// A free-text filed date as the calendar prints it: a date the parser
/// reads is printed as `18 Dec 26`; anything else is shown as recorded.
#[cfg(feature = "server")]
fn filed_date(raw: &str) -> String {
    let raw = raw.trim();
    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%m/%d/%Y"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%m/%d/%y"))
        .map_or_else(
            |_| raw.to_string(),
            |date| date.format("%-d %b %y").to_string(),
        )
}

/// The docket entries and open deadlines of one matter as calendar rows,
/// soonest first. An entry dated by the court (a hearing or trial) sorts by
/// that date; a filing sorts by the date it was filed; an undated entry
/// sorts first, so it is never hidden below the fold.
#[cfg(feature = "server")]
#[must_use]
pub fn timeline_events(
    entries: &[store::cases::DocketEntry],
    deadlines: &[store::project_deadlines::Deadline],
    project: &str,
    entity: &str,
) -> Vec<CalendarEvent> {
    let superseded: std::collections::HashSet<uuid::Uuid> = entries
        .iter()
        .filter_map(|entry| entry.supersedes)
        .collect();
    let mut rows: Vec<(Option<chrono::DateTime<chrono::Utc>>, String, CalendarEvent)> = entries
        .iter()
        .map(|entry| {
            let scheduled = entry.scheduled_on;
            let filed = entry
                .filed_or_served_on
                .as_deref()
                .and_then(parse_filed_date);
            let sort_date = scheduled.or(filed.map(|date| {
                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                    date.and_hms_opt(0, 0, 0).unwrap_or_default(),
                    chrono::Utc,
                )
            }));
            let date = entry.scheduled_on.map_or_else(
                || {
                    entry
                        .filed_or_served_on
                        .as_deref()
                        .map(filed_date)
                        .unwrap_or_default()
                },
                |when| when.format("%-d %b %y %H:%M UTC").to_string(),
            );
            let event = CalendarEvent {
                date: date.clone(),
                sort_date: sort_date.map(|when| when.to_rfc3339()),
                event: format!("D.I. {} · {}", entry.entry_number, entry.title),
                status: entry_status(entry, superseded.contains(&entry.id)),
                project: project.to_string(),
                entity: entity.to_string(),
            };
            (sort_date, date, event)
        })
        .collect();
    for deadline in deadlines
        .iter()
        .filter(|row| row.status == store::project_deadlines::STATUS_OPEN)
    {
        let due = chrono::DateTime::parse_from_rfc3339(&deadline.due_at).ok();
        let due_sort_date = due.map(|due| due.with_timezone(&chrono::Utc));
        let date = due.map_or_else(
            || deadline.due_at.clone(),
            |due| {
                due.with_timezone(&chrono::Utc)
                    .format("%-d %b %y %H:%M UTC")
                    .to_string()
            },
        );
        let authority = if deadline.authority.trim().is_empty() {
            crate::project_deadlines::authority_kind_label(&deadline.authority_kind).to_string()
        } else {
            deadline.authority.clone()
        };
        let event = CalendarEvent {
            date: date.clone(),
            sort_date: due_sort_date.map(|due| due.to_rfc3339()),
            event: authority,
            status: format!(
                "Deadline · {}",
                crate::project_deadlines::role_label(&deadline.calendar_role)
            ),
            project: project.to_string(),
            entity: entity.to_string(),
        };
        rows.push((due_sort_date, date, event));
    }
    rows.sort_by(|(a, a_display, _), (b, b_display, _)| {
        a.cmp(b).then_with(|| a_display.cmp(b_display))
    });
    rows.into_iter().map(|(_, _, event)| event).collect()
}

#[cfg(feature = "server")]
fn parse_filed_date(raw: &str) -> Option<chrono::NaiveDate> {
    let raw = raw.trim();
    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%m/%d/%Y"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(raw, "%m/%d/%y"))
        .ok()
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::timeline_events;
    use store::cases::DocketEntry;
    use store::project_deadlines::Deadline;
    use uuid::Uuid;

    fn entry(
        number: &str,
        kind: &str,
        filed: Option<&str>,
        scheduled: Option<&str>,
    ) -> DocketEntry {
        DocketEntry {
            id: Uuid::now_v7(),
            case_id: Uuid::nil(),
            entry_number: number.to_string(),
            kind: kind.to_string(),
            title: format!("Entry {number}"),
            party: None,
            filed_or_served_on: filed.map(str::to_string),
            scheduled_on: scheduled.map(|s| s.parse().unwrap()),
            supersedes: None,
            document_asset_id: None,
            notation_id: None,
            stamp_case_number: None,
            stamp_entry_number: None,
            stamp_filed_on: None,
            integrity_checked_at: None,
            integrity_source_sha256_hex: None,
            integrity_matched: None,
            inserted_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn deadline(due_at: &str, status: &str) -> Deadline {
        Deadline {
            id: Uuid::now_v7(),
            project_id: Uuid::nil(),
            title: None,
            description: None,
            due_at: due_at.to_string(),
            authority_kind: "court_order".to_string(),
            authority: "Order setting briefing schedule".to_string(),
            source: "lawyer".to_string(),
            source_document_id: None,
            owner_person_id: None,
            status: status.to_string(),
            calendar_role: "firm".to_string(),
            category: Some("court".to_string()),
            internal_lead_days: Some(7),
            client_lead_days: None,
            replay_key: None,
            inserted_at: String::new(),
            updated_at: String::new(),
            calendar_sequence: None,
        }
    }

    #[test]
    fn entries_and_open_deadlines_interleave_in_date_order() {
        let mut continued = entry("14", "hearing", None, Some("2027-04-01T09:00:00Z"));
        let prior = entry("13", "hearing", None, Some("2027-03-15T09:00:00Z"));
        continued.supersedes = Some(prior.id);
        let rows = timeline_events(
            &[
                entry("12", "motion", Some("3/5/2026"), None),
                prior,
                continued,
            ],
            &[
                deadline("2026-11-02T17:00:00-08:00", "open"),
                deadline("2026-01-01T17:00:00-08:00", "complete"),
            ],
            "Sample matter",
            "Sample LLC",
        );
        let cells: Vec<(&str, &str)> = rows
            .iter()
            .map(|row| (row.date.as_str(), row.status.as_str()))
            .collect();
        assert_eq!(
            cells,
            [
                ("5 Mar 26", "Source pending"),
                ("3 Nov 26 01:00 UTC", "Deadline · Our deadline"),
                ("15 Mar 27 09:00 UTC", "Superseded"),
                ("1 Apr 27 09:00 UTC", "Hearing"),
            ]
        );
        assert_eq!(rows[0].event, "D.I. 12 · Entry 12");
        assert_eq!(rows[1].event, "Order setting briefing schedule");
    }

    #[test]
    fn an_undated_entry_sorts_first_and_is_never_hidden() {
        let rows = timeline_events(
            &[entry("1", "other", None, None)],
            &[deadline("2026-11-02T17:00:00-08:00", "open")],
            "m",
            "e",
        );
        assert_eq!(rows[0].event, "D.I. 1 · Entry 1");
        assert_eq!(rows[0].date, "");
    }
}
