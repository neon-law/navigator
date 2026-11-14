use chrono::{DateTime, Days, FixedOffset, NaiveDate, Utc};
use dioxus::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Entry {
    id: String,
    project: String,
    title: String,
    date: Date,
    recorded: DateTime<Utc>,
    authority: String,
    notes: String,
    source: String,
    deadline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Date {
    Instant(DateTime<FixedOffset>),
    Day(NaiveDate),
    Unknown,
}

impl Date {
    #[cfg(feature = "server")]
    fn parse(value: &str) -> Option<Self> {
        DateTime::parse_from_rfc3339(value)
            .map(Self::Instant)
            .or_else(|_| NaiveDate::parse_from_str(value, "%Y-%m-%d").map(Self::Day))
            .ok()
    }

    fn label(&self) -> String {
        match self {
            Self::Instant(at) => at.format("%Y-%m-%d %H:%M %:z").to_string(),
            Self::Day(day) => day.to_string(),
            Self::Unknown => "Date not recorded".into(),
        }
    }

    fn order(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Instant(at) => Some(at.to_utc()),
            Self::Day(day) => Some(day.and_hms_opt(0, 0, 0)?.and_utc()),
            Self::Unknown => None,
        }
    }
}

fn band(date: &Date, now: DateTime<Utc>) -> &'static str {
    let Date::Instant(at) = date else {
        return "Upcoming";
    };
    if *at < now {
        return "Overdue";
    }
    let today = now.with_timezone(at.offset()).date_naive();
    let day = at.date_naive();
    if day == today {
        "Today"
    } else if today
        .checked_add_days(Days::new(7))
        .is_some_and(|end| day <= end)
    {
        "Next 7 days"
    } else if today
        .checked_add_days(Days::new(30))
        .is_some_and(|end| day <= end)
    {
        "Next 30 days"
    } else {
        "Later"
    }
}

#[cfg(feature = "server")]
pub(super) async fn load(
    db: &store::surreal::SurrealDb,
    projects: &[store::projects::Project],
) -> Result<Vec<Entry>, ServerFnError> {
    use store::{cases, project_deadlines};

    let mut entries = Vec::new();
    for project in projects.iter().filter(|p| p.status == "open") {
        let project_href = format!("/app/projects/{}", project.code);
        let (deadlines, docket) = futures::try_join!(
            async {
                project_deadlines::list(db, project.id)
                    .await
                    .map_err(|_| ())
            },
            async {
                cases::docket_for_project(db, project.id)
                    .await
                    .map_err(|_| ())
            },
        )
        .map_err(|()| ServerFnError::new("Could not load the Workbench timeline"))?;
        for row in deadlines
            .into_iter()
            .filter(|row| row.status == project_deadlines::STATUS_OPEN)
        {
            let date = Date::parse(&row.due_at)
                .ok_or_else(|| ServerFnError::new("A Deadline has an unreadable due date"))?;
            let recorded = DateTime::parse_from_rfc3339(&row.updated_at)
                .map(|at| at.to_utc())
                .or_else(|_| DateTime::parse_from_rfc3339(&row.due_at).map(|at| at.to_utc()))
                .map_err(|_| ServerFnError::new("A Deadline has an unreadable timestamp"))?;
            entries.push(Entry {
                id: format!("deadline-{}", row.id),
                project: project.code.clone(),
                title: row.title.unwrap_or_else(|| {
                    if row.calendar_role == "hearing" {
                        "Hearing".into()
                    } else {
                        "Deadline".into()
                    }
                }),
                date,
                recorded,
                authority: if row.authority.trim().is_empty() {
                    crate::project_deadlines::authority_kind_label(&row.authority_kind).into()
                } else {
                    row.authority
                },
                notes: deadline_notes(row.description.unwrap_or_default()),
                source: row.source_document_id.map_or_else(
                    || format!("{project_href}#deadline-{}", row.id),
                    |id| format!("{project_href}/documents/{id}"),
                ),
                deadline: true,
            });
        }
        entries.extend(docket.into_iter().map(|row| {
            Entry {
                id: format!("docket-{}", row.id),
                project: project.code.clone(),
                title: format!("D.I. {} · {}", row.entry_number, row.title),
                date: row
                    .filed_or_served_on
                    .as_deref()
                    .and_then(Date::parse)
                    .unwrap_or(Date::Unknown),
                recorded: row.updated_at,
                authority: String::new(),
                notes: String::new(),
                source: row.document_asset_id.map_or_else(
                    || format!("/app/api/projects/{}/docket", project.id),
                    |id| format!("{project_href}/documents/{id}"),
                ),
                deadline: false,
            }
        }));
    }
    Ok(entries)
}

#[cfg(feature = "server")]
fn deadline_notes(description: String) -> String {
    serde_json::from_str::<serde_json::Value>(&description)
        .ok()
        .filter(|value| value.get("deadline_proposal").is_some())
        .map_or(description, |value| {
            value["notes"].as_str().unwrap_or_default().to_owned()
        })
}

#[component]
pub(super) fn WorkbenchTimeline(entries: Vec<Entry>, now: DateTime<Utc>) -> Element {
    let mut deadlines: Vec<_> = entries.iter().filter(|entry| entry.deadline).collect();
    deadlines.sort_by_key(|entry| (entry.date.order(), &entry.id));
    let mut docket: Vec<_> = entries.iter().filter(|entry| !entry.deadline).collect();
    docket.sort_by_key(|entry| (std::cmp::Reverse(entry.date.order()), &entry.id));
    let calendar = ics(&entries);
    let download =
        percent_encoding::utf8_percent_encode(&calendar, percent_encoding::NON_ALPHANUMERIC);
    rsx! {
        section { class: "project-deadlines", aria_labelledby: "workbench-timeline",
            h2 { id: "workbench-timeline", "Matter timeline" }
            if entries.is_empty() {
                p { class: "nav-muted", "No Deadlines or Docket Entries on your open matters." }
            } else {
                a { href: "data:text/calendar;charset=utf-8,{download}", download: "matter-timeline.ics", "Download calendar (.ics)" }
                for label in ["Overdue", "Today", "Next 7 days", "Next 30 days", "Later", "Upcoming"] {
                    if deadlines.iter().any(|entry| band(&entry.date, now) == label) {
                        h3 { "{label}" }
                        ul {
                            for entry in deadlines.iter().filter(|entry| band(&entry.date, now) == label) {
                                TimelineRow { key: "{entry.id}", entry: (*entry).clone() }
                            }
                        }
                    }
                }
                if !docket.is_empty() {
                    h3 { "Docket Entries" }
                    ul {
                        for entry in docket {
                            TimelineRow { key: "{entry.id}", entry: entry.clone() }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn TimelineRow(entry: Entry) -> Element {
    let date = entry.date.label();
    rsx! {
        li {
            "{date} · "
            a { href: "{entry.source}", "{entry.title}" }
            " · "
            a { href: "/app/projects/{entry.project}", "{entry.project}" }
            if !entry.authority.is_empty() { p { "{entry.authority}" } }
            if !entry.notes.is_empty() { p { "{entry.notes}" } }
        }
    }
}

fn ics(entries: &[Entry]) -> String {
    fn escape(text: &str) -> String {
        text.replace('\\', "\\\\")
            .replace('\r', "")
            .replace('\n', "\\n")
            .replace(',', "\\,")
            .replace(';', "\\;")
    }
    fn line(out: &mut String, content: &str) {
        let mut octets = 0;
        for ch in content.chars() {
            if octets + ch.len_utf8() > 75 {
                out.push_str("\r\n ");
                octets = 1;
            }
            out.push(ch);
            octets += ch.len_utf8();
        }
        out.push_str("\r\n");
    }
    let mut out =
        String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Neon Law//Navigator//EN\r\n");
    for entry in entries {
        let start = match entry.date {
            Date::Instant(at) => format!("DTSTART:{}", at.to_utc().format("%Y%m%dT%H%M%SZ")),
            Date::Day(day) => format!("DTSTART;VALUE=DATE:{}", day.format("%Y%m%d")),
            Date::Unknown => continue,
        };
        for field in [
            "BEGIN:VEVENT".into(),
            format!("UID:{}@navigator", entry.id),
            format!("DTSTAMP:{}", entry.recorded.format("%Y%m%dT%H%M%SZ")),
            start,
            format!(
                "SUMMARY:{}",
                escape(&format!("{} · {}", entry.project, entry.title))
            ),
            format!(
                "DESCRIPTION:{}",
                escape(&format!("{}\n{}", entry.authority, entry.notes))
            ),
            "END:VEVENT".into(),
        ] {
            line(&mut out, &field);
        }
    }
    out.push_str("END:VCALENDAR\r\n");
    out
}

#[cfg(all(test, feature = "server"))]
mod tests {
    use super::*;

    #[test]
    fn workbench_shows_the_recorded_condition_and_exports_the_same_dates() {
        let now = DateTime::parse_from_rfc3339("2028-05-01T12:00:00Z")
            .unwrap()
            .to_utc();
        let entry = Entry {
            id: "deadline-fixture".into(),
            project: "synthetic-matter".into(),
            title: "Response deadline".into(),
            date: Date::parse("2028-05-01T15:00:00-04:00").unwrap(),
            recorded: now,
            authority: "Synthetic scheduling order".into(),
            notes: "Moves to May 6 if the order is entered.".into(),
            source: "/app/projects/synthetic-matter#deadline-fixture".into(),
            deadline: true,
        };
        let docket = Entry {
            id: "docket-fixture".into(),
            title: "D.I. 1 · Synthetic filing".into(),
            date: Date::parse("2028-04-30").unwrap(),
            deadline: false,
            ..entry.clone()
        };
        let entries = vec![entry, docket];
        let calendar = ics(&entries);
        assert!(calendar.contains("DTSTART:20280501T190000Z\r\n"));
        assert!(calendar.contains("DTSTART;VALUE=DATE:20280430\r\n"));
        let html = dioxus_ssr::render_element(rsx! { WorkbenchTimeline { entries, now } });
        for expected in [
            "Today",
            "2028-05-01 15:00 -04:00",
            "Moves to May 6 if the order is entered.",
            "2028-04-30",
            "Synthetic scheduling order",
            "href=\"/app/projects/synthetic-matter#deadline-fixture\"",
            "download=\"matter-timeline.ics\"",
        ] {
            assert!(html.contains(expected), "missing {expected}");
        }
    }
}
