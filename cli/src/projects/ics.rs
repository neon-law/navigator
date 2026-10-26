//! RFC 5545 rendering of a Project's Deadlines as one `VCALENDAR`.
//!
//! The renderer is pure: the same rows always produce the same bytes. A
//! Deadline's `UID` is `<deadline id>@<host>`, so a regenerated file updates the
//! calendar event instead of duplicating it. `SEQUENCE` is its stored monotonic
//! `calendar_sequence`; `DTSTAMP` follows `updated_at`. Historical rows without a
//! sequence retain their timestamp-based version until the next write increments
//! it. A cancelled row renders `STATUS:CANCELLED`.
//!
//! The stored `due_at` carries only a UTC offset, so the event's zone is a
//! fixed-offset `VTIMEZONE` named for that offset (`UTC-08:00`). A `Z` offset
//! renders as UTC time.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use chrono::{DateTime, FixedOffset, Utc};
use serde_json::Value;

/// Content lines are folded so none exceeds this many octets, excluding CRLF.
const MAX_LINE_OCTETS: usize = 75;

/// Where the calendar points back to: the deployment host (the `UID` suffix),
/// the matter-page link, and the Project code that names each event.
pub struct Calendar<'a> {
    pub host: &'a str,
    pub base_url: &'a str,
    pub project_code: &'a str,
}

/// Render every Deadline row as one `VCALENDAR`, ordered by due instant then id.
/// A row without a parseable `id` or `due_at` is an error: a silently skipped
/// Deadline is a missed reminder.
pub fn render(calendar: &Calendar<'_>, rows: &[Value]) -> Result<String, String> {
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        events.push(Event::from_row(row)?);
    }
    events.sort_by(|a, b| (a.due, &a.id).cmp(&(b.due, &b.id)));

    let mut zones: BTreeMap<i32, String> = BTreeMap::new();
    for event in &events {
        let offset = event.due.offset().local_minus_utc();
        if offset != 0 {
            zones.entry(offset).or_insert_with(|| zone_name(offset));
        }
    }

    let mut out = Out::default();
    out.line("BEGIN:VCALENDAR");
    out.line("VERSION:2.0");
    out.line("PRODID:-//Neon Law//Navigator//EN");
    out.line("CALSCALE:GREGORIAN");
    out.line("METHOD:PUBLISH");
    for (offset, name) in &zones {
        out.timezone(*offset, name);
    }
    for event in &events {
        out.event(calendar, event);
    }
    out.line("END:VCALENDAR");
    Ok(out.0)
}

struct Event {
    id: String,
    title: String,
    location: Option<String>,
    due: DateTime<FixedOffset>,
    stamp: DateTime<Utc>,
    sequence: i64,
    status: String,
    authority_kind: String,
    authority: String,
    calendar_role: String,
    source_document_id: Option<String>,
    internal_lead_days: Option<i64>,
}

impl Event {
    fn from_row(row: &Value) -> Result<Self, String> {
        let text = |key: &str| row[key].as_str().unwrap_or_default().to_string();
        let id = text("id");
        if id.is_empty() {
            return Err("a deadline row has no id".into());
        }
        let due = DateTime::parse_from_rfc3339(&text("due_at"))
            .map_err(|error| format!("deadline {id} has an unreadable due_at: {error}"))?;
        // `updated_at` is the row's version; without one, the due instant stands in.
        let (stamp, sequence) = match DateTime::parse_from_rfc3339(&text("updated_at")) {
            Ok(updated) => (updated.with_timezone(&Utc), updated.timestamp().max(0)),
            Err(_) => (due.with_timezone(&Utc), 0),
        };
        let sequence = match row
            .get("calendar_sequence")
            .filter(|value| !value.is_null())
        {
            Some(value) => value
                .as_i64()
                .ok_or_else(|| format!("deadline {id} has an unreadable calendar_sequence"))?,
            None => sequence,
        };
        if !(0..=i64::from(i32::MAX)).contains(&sequence) {
            return Err(format!(
                "deadline {id} has a calendar_sequence outside the RFC 5545 integer range"
            ));
        }
        Ok(Self {
            id,
            title: text("title").trim().to_string(),
            location: row["location"].as_str().map(str::to_string),
            due,
            stamp,
            sequence,
            status: text("status"),
            authority_kind: text("authority_kind"),
            authority: text("authority"),
            calendar_role: text("calendar_role"),
            source_document_id: row["source_document_id"].as_str().map(str::to_string),
            internal_lead_days: row["internal_lead_days"].as_i64(),
        })
    }

    fn summary(&self, project_code: &str) -> String {
        let action = if !self.title.is_empty() {
            self.title.clone()
        } else if self.calendar_role == "hearing" {
            "Hearing".to_string()
        } else {
            let kind = match self.authority_kind.as_str() {
                "statute" => "Statutory deadline",
                "court_rule" => "Court rule deadline",
                "court_order" => "Court order deadline",
                "contract" => "Contract deadline",
                _ => "Internal deadline",
            };
            if self.calendar_role == "opponent" {
                format!("{kind} (opponent)")
            } else {
                kind.to_string()
            }
        };
        format!("{action} - {project_code}")
    }
}

fn zone_name(offset_seconds: i32) -> String {
    format!("UTC{}", offset_label(offset_seconds, true))
}

/// `+hh:mm`, or `+hhmm` for the `TZOFFSET` properties.
fn offset_label(offset_seconds: i32, colon: bool) -> String {
    let sign = if offset_seconds < 0 { '-' } else { '+' };
    let minutes = offset_seconds.abs() / 60;
    let (h, m) = (minutes / 60, minutes % 60);
    if colon {
        format!("{sign}{h:02}:{m:02}")
    } else {
        format!("{sign}{h:02}{m:02}")
    }
}

/// RFC 5545 TEXT escaping.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\r' => {}
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

#[derive(Default)]
struct Out(String);

impl Out {
    /// Append one content line, folded at 75 octets on a character boundary,
    /// each continuation led by one space and ended by CRLF.
    fn line(&mut self, content: &str) {
        let mut used = 0;
        for c in content.chars() {
            let width = c.len_utf8();
            if used + width > MAX_LINE_OCTETS {
                self.0.push_str("\r\n ");
                used = 1;
            }
            self.0.push(c);
            used += width;
        }
        self.0.push_str("\r\n");
    }

    fn timezone(&mut self, offset: i32, name: &str) {
        let label = offset_label(offset, false);
        self.line("BEGIN:VTIMEZONE");
        self.line(&format!("TZID:{name}"));
        self.line("BEGIN:STANDARD");
        self.line("DTSTART:19700101T000000");
        self.line(&format!("TZOFFSETFROM:{label}"));
        self.line(&format!("TZOFFSETTO:{label}"));
        self.line(&format!("TZNAME:{name}"));
        self.line("END:STANDARD");
        self.line("END:VTIMEZONE");
    }

    fn event(&mut self, calendar: &Calendar<'_>, event: &Event) {
        let summary = event.summary(calendar.project_code);
        self.line("BEGIN:VEVENT");
        self.line(&format!("UID:{}@{}", event.id, calendar.host));
        self.line(&format!("DTSTAMP:{}", event.stamp.format("%Y%m%dT%H%M%SZ")));
        self.line(&format!("SEQUENCE:{}", event.sequence));
        let offset = event.due.offset().local_minus_utc();
        if offset == 0 {
            self.line(&format!(
                "DTSTART:{}",
                event.due.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
            ));
        } else {
            self.line(&format!(
                "DTSTART;TZID=\"{}\":{}",
                zone_name(offset),
                event.due.format("%Y%m%dT%H%M%S")
            ));
        }
        self.line(&format!("SUMMARY:{}", escape(&summary)));
        if let Some(location) = &event.location {
            self.line(&format!("LOCATION:{}", escape(location)));
        }
        let mut description = String::new();
        if !event.authority.is_empty() {
            let _ = writeln!(description, "Authority: {}", event.authority);
        }
        if let Some(document) = &event.source_document_id {
            let _ = writeln!(description, "Source document: {document}");
        }
        let _ = writeln!(description, "Deadline: {}", event.id);
        let _ = write!(
            description,
            "Matter page: {}/app/projects/{}",
            calendar.base_url.trim_end_matches('/'),
            calendar.project_code
        );
        self.line(&format!("DESCRIPTION:{}", escape(&description)));
        self.line(if event.status == "cancelled" {
            "STATUS:CANCELLED"
        } else {
            "STATUS:CONFIRMED"
        });
        self.line("TRANSP:TRANSPARENT");
        if let (true, Some(days)) = (event.status == "open", event.internal_lead_days) {
            self.line("BEGIN:VALARM");
            self.line("ACTION:DISPLAY");
            self.line(&format!("DESCRIPTION:{}", escape(&summary)));
            self.line(&format!("TRIGGER:-P{days}D"));
            self.line("END:VALARM");
        }
        self.line("END:VEVENT");
    }
}

#[cfg(test)]
mod tests {
    use super::{render, Calendar};
    use serde_json::{json, Value};

    const ID: &str = "0190a000-0000-7000-8000-000000000001";

    fn calendar() -> Calendar<'static> {
        Calendar {
            host: "navigator.example.test",
            base_url: "https://navigator.example.test/",
            project_code: "acme",
        }
    }

    fn row() -> Value {
        json!({
            "id": ID,
            "project_id": "0190a000-0000-7000-8000-0000000000ff",
            "due_at": "2026-11-02T17:00:00-08:00",
            "authority_kind": "court_order",
            "authority": "Order setting briefing, entered 2026-10-05",
            "source_document_id": "0190a000-0000-7000-8000-0000000000aa",
            "status": "open",
            "calendar_role": "firm",
            "internal_lead_days": 7,
            "updated_at": "2026-10-06T00:00:00+00:00"
        })
    }

    fn lines(text: &str) -> Vec<&str> {
        text.strip_suffix("\r\n").unwrap().split("\r\n").collect()
    }

    #[test]
    fn renders_the_required_properties_for_one_deadline() {
        let text = render(&calendar(), &[row()]).unwrap();
        let all = lines(&text);
        for expected in [
            "BEGIN:VCALENDAR",
            "VERSION:2.0",
            "BEGIN:VTIMEZONE",
            "TZID:UTC-08:00",
            "TZOFFSETTO:-0800",
            "BEGIN:VEVENT",
            "UID:0190a000-0000-7000-8000-000000000001@navigator.example.test",
            "DTSTAMP:20261006T000000Z",
            "DTSTART;TZID=\"UTC-08:00\":20261102T170000",
            "SUMMARY:Court order deadline - acme",
            "STATUS:CONFIRMED",
            "BEGIN:VALARM",
            "TRIGGER:-P7D",
            "END:VEVENT",
            "END:VCALENDAR",
        ] {
            assert!(all.contains(&expected), "missing {expected}\n{text}");
        }
        assert!(text.contains("PRODID:"));
        assert!(text.replace("\r\n ", "").contains(
            "DESCRIPTION:Authority: Order setting briefing\\, entered 2026-10-05\\nSource document: 0190a000-0000-7000-8000-0000000000aa\\nDeadline: 0190a000-0000-7000-8000-000000000001\\nMatter page: https://navigator.example.test/app/projects/acme"
        ));
    }

    #[test]
    fn every_line_ends_in_crlf_and_none_exceeds_75_octets() {
        let mut long = row();
        long["authority"] = json!("Synthetic authority ".repeat(20) + "é§é§é§é§é§é§é§é§");
        let text = render(&calendar(), &[long]).unwrap();
        assert!(!text.replace("\r\n", "").contains('\n'));
        assert!(!text.replace("\r\n", "").contains('\r'));
        for line in lines(&text) {
            assert!(line.len() <= 75, "{} octets: {line}", line.len());
        }
        let unfolded = text.replace("\r\n ", "");
        assert!(
            unfolded.contains("é§é§é§é§é§é§é§é§"),
            "multibyte text survives folding"
        );
    }

    #[test]
    fn an_update_keeps_the_uid_and_raises_the_sequence() {
        let first = render(&calendar(), &[row()]).unwrap();
        let mut edited = row();
        edited["due_at"] = json!("2026-11-09T17:00:00-08:00");
        edited["updated_at"] = json!("2026-10-07T00:00:00+00:00");
        let second = render(&calendar(), &[edited]).unwrap();
        let uid = |t: &str| {
            lines(t)
                .into_iter()
                .find(|l| l.starts_with("UID:"))
                .unwrap()
                .to_string()
        };
        let seq = |t: &str| {
            lines(t)
                .into_iter()
                .find_map(|l| l.strip_prefix("SEQUENCE:"))
                .unwrap()
                .parse::<i64>()
                .unwrap()
        };
        assert_eq!(uid(&first), uid(&second));
        assert!(seq(&second) > seq(&first));
        assert_eq!(
            first,
            render(&calendar(), &[row()]).unwrap(),
            "rendering is deterministic"
        );
    }

    #[test]
    fn a_cancelled_row_is_cancelled_and_carries_no_alarm() {
        let mut cancelled = row();
        cancelled["status"] = json!("cancelled");
        let text = render(&calendar(), &[cancelled]).unwrap();
        assert!(text.contains("STATUS:CANCELLED\r\n"));
        assert!(!text.contains("VALARM"));
    }

    #[test]
    fn utc_rows_render_in_utc_without_a_timezone_block() {
        let mut utc = row();
        utc["due_at"] = json!("2026-11-03T01:00:00Z");
        utc["internal_lead_days"] = Value::Null;
        let text = render(&calendar(), &[utc]).unwrap();
        assert!(text.contains("DTSTART:20261103T010000Z\r\n"));
        assert!(!text.contains("VTIMEZONE"));
        assert!(!text.contains("VALARM"));
    }

    #[test]
    fn events_sort_by_due_instant_and_a_bad_row_is_refused() {
        let mut earlier = row();
        earlier["id"] = json!("0190a000-0000-7000-8000-000000000002");
        earlier["due_at"] = json!("2026-10-01T09:00:00-07:00");
        let text = render(&calendar(), &[row(), earlier]).unwrap();
        let first = text.find("000000000002@").unwrap();
        let second = text.find("000000000001@").unwrap();
        assert!(first < second);

        let mut bad = row();
        bad["due_at"] = json!("2026-11-02");
        assert!(render(&calendar(), &[bad]).is_err());
    }

    #[test]
    fn the_summary_names_the_project_code_and_nothing_about_the_client() {
        let mut hearing = row();
        hearing["calendar_role"] = json!("hearing");
        let text = render(&calendar(), &[hearing]).unwrap();
        assert!(text.contains("SUMMARY:Hearing - acme\r\n"));
    }

    #[test]
    fn a_title_names_the_event_and_a_row_without_one_keeps_the_kind() {
        let mut titled = row();
        titled["title"] = json!("Answer to the complaint");
        let text = render(&calendar(), &[titled]).unwrap();
        assert!(text.contains("SUMMARY:Answer to the complaint - acme\r\n"));
        let mut untitled = row();
        untitled["title"] = json!(null);
        let text = render(&calendar(), &[untitled]).unwrap();
        assert!(text.contains("SUMMARY:Court order deadline - acme\r\n"));
    }
    #[test]
    fn persisted_sequences_version_same_second_edits_and_fit_rfc_integers() {
        let mut original = row();
        original["calendar_sequence"] = json!(0);
        let first = render(&calendar(), &[original.clone()]).unwrap();
        original["calendar_sequence"] = json!(1);
        let next = render(&calendar(), &[original.clone()]).unwrap();
        assert!(first.contains("SEQUENCE:0\r\n"));
        assert!(next.contains("SEQUENCE:1\r\n"));
        let uid = first.lines().find(|line| line.starts_with("UID:")).unwrap();
        assert!(next.lines().any(|line| line == uid));
        original["status"] = json!("cancelled");
        original["calendar_sequence"] = json!(2);
        let cancelled = render(&calendar(), &[original.clone()]).unwrap();
        assert!(cancelled.contains("STATUS:CANCELLED\r\n"));
        assert!(cancelled.contains("SEQUENCE:2\r\n"));
        assert!(cancelled.lines().any(|line| line == uid));
        for invalid in [json!(-1), json!(2_147_483_648_i64), json!("bad")] {
            original["calendar_sequence"] = invalid;
            assert!(render(&calendar(), &[original.clone()]).is_err());
        }
    }
}
