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
    for row in rows
        .iter()
        .filter(|row| row["calendar_review_pending"] != true)
    {
        events.push(Event::from_row(row)?);
    }
    events.sort_by(|a, b| (a.due, &a.id).cmp(&(b.due, &b.id)));

    let mut zones: BTreeMap<i32, String> = BTreeMap::new();
    for event in &events {
        let offset = event.due.offset().local_minus_utc();
        if event.due_on.is_none() && offset != 0 {
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
    description: String,
    location: Option<String>,
    due: DateTime<FixedOffset>,
    due_on: Option<model::project_deadlines::DueOn>,
    hearing: Option<model::project_deadlines::Hearing>,
    scheduling_policy: model::project_deadlines::SchedulingPolicy,
    category: Option<model::project_deadlines::Category>,
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
            description: text("description"),
            location: row["location"].as_str().map(str::to_string),
            due,
            hearing: serde_json::from_value(row["hearing"].clone())
                .map_err(|error| format!("invalid hearing: {error}"))?,
            due_on: serde_json::from_value(row["due_on"].clone())
                .map_err(|error| format!("invalid civil date: {error}"))?,
            scheduling_policy: row.get("scheduling_policy").map_or_else(
                || Ok(model::project_deadlines::SchedulingPolicy::default()),
                |policy| {
                    serde_json::from_value(policy.clone())
                        .map_err(|error| format!("invalid scheduling policy: {error}"))
                },
            )?,
            category: row["category"]
                .as_str()
                .and_then(|value| value.parse().ok()),
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
        model::project_deadlines::calendar_summary(
            project_code,
            &self.title,
            &self.calendar_role,
            &self.authority_kind,
            self.category,
            &self.status,
        )
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

    #[allow(clippy::too_many_lines)]
    fn event(&mut self, calendar: &Calendar<'_>, event: &Event) {
        let summary = event.summary(calendar.project_code);
        self.line("BEGIN:VEVENT");
        self.line(&format!("UID:{}@{}", event.id, calendar.host));
        self.line(&format!("DTSTAMP:{}", event.stamp.format("%Y%m%dT%H%M%SZ")));
        self.line(&format!("SEQUENCE:{}", event.sequence));
        let offset = event.due.offset().local_minus_utc();
        if let Some(day) = &event.due_on {
            self.line(&format!("DTSTART;VALUE=DATE:{}", day.date.format("%Y%m%d")));
        } else if offset == 0 {
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
        let hearing = model::project_deadlines::is_appearance(&event.calendar_role, event.category);
        let policy = event.scheduling_policy;
        let duration = event
            .hearing
            .as_ref()
            .map_or(60, model::project_deadlines::Hearing::minutes);
        let open = event.status == "open";
        if hearing {
            self.line(&format!("DURATION:PT{duration}M"));
        }
        self.line(&format!("SUMMARY:{}", escape(&summary)));
        if let Some(location) = event.location.as_ref().or_else(|| {
            event
                .hearing
                .as_ref()
                .and_then(|details| details.location.as_ref())
        }) {
            self.line(&format!("LOCATION:{}", escape(location)));
        }
        let mut description = event.description.clone();
        if !description.is_empty() {
            description.push('\n');
        }
        if let Some(cutoff) = event
            .due_on
            .as_ref()
            .and_then(model::project_deadlines::DueOn::cutoff_description)
        {
            let _ = writeln!(description, "{cutoff}");
        }
        let submit_by = (open && !hearing)
            .then(|| policy.submit_by(event.due, event.due_on.as_ref()))
            .flatten();
        if let Some(at) = submit_by {
            let _ = writeln!(description, "Submit by {}", at.to_rfc3339());
        }
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
        self.line(if hearing {
            "TRANSP:OPAQUE"
        } else {
            "TRANSP:TRANSPARENT"
        });
        if let Some(at) = submit_by {
            self.line("BEGIN:VALARM");
            self.line("ACTION:DISPLAY");
            self.line(&format!("DESCRIPTION:Submit by — {}", escape(&summary)));
            self.line(&format!(
                "TRIGGER;VALUE=DATE-TIME:{}",
                at.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
            ));
            self.line("END:VALARM");
        }
        for days in model::project_deadlines::reminder_days(
            &event.status,
            event.category,
            event.internal_lead_days,
        ) {
            self.line("BEGIN:VALARM");
            self.line("ACTION:DISPLAY");
            self.line(&format!("DESCRIPTION:{}", escape(&summary)));
            self.line(&format!("TRIGGER:-P{days}D"));
            self.line("END:VALARM");
        }
        self.line("END:VEVENT");
        if hearing {
            self.buffer_event(calendar, event, &summary, duration);
        }
    }

    fn buffer_event(
        &mut self,
        calendar: &Calendar<'_>,
        event: &Event,
        summary: &str,
        duration: u32,
    ) {
        // Keep the scheduled appearance intact and publish its busy window separately.
        let policy = event.scheduling_policy;
        let open = event.status == "open";
        if let Some((from, until)) = policy.appearance(event.due, duration) {
            self.line("BEGIN:VEVENT");
            self.line(&format!("UID:{}-buffer@{}", event.id, calendar.host));
            self.line(&format!("DTSTAMP:{}", event.stamp.format("%Y%m%dT%H%M%SZ")));
            self.line(&format!("SEQUENCE:{}", event.sequence));
            self.line(&format!(
                "DTSTART:{}",
                from.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
            ));
            self.line(&format!(
                "DTEND:{}",
                until.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
            ));
            self.line(&format!(
                "SUMMARY:{}",
                escape(&format!("{summary} (reserved time)"))
            ));
            self.line(if open {
                "STATUS:CONFIRMED"
            } else {
                "STATUS:CANCELLED"
            });
            self.line("TRANSP:OPAQUE");
            self.line("END:VEVENT");
        }
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
    fn civil_deadline_exports_on_its_local_day_with_court_reminders() {
        let row = serde_json::json!({"id":"date-1","due_at":"2028-07-13T06:59:00Z","due_on":{"date":"2028-07-12","timezone":"America/Los_Angeles","cutoff":"23:59:00"},"title":"File response","category":"court","status":"open"});
        let result = super::render(
            &super::Calendar {
                host: "example.test",
                base_url: "https://example.test",
                project_code: "sample",
            },
            &[row],
        )
        .unwrap();
        assert!(result.contains("DTSTART;VALUE=DATE:20280712\r\n"));
        assert!(result.contains("Cutoff: 23:59:00 America/Los_Angeles"));
        assert!(result.contains("SUMMARY:sample: DEADLINE – File response"));
        assert!(result.contains("TRIGGER;VALUE=DATE-TIME:20280713T064400Z"));
        for lead in [7, 3, 1] {
            assert!(result.contains(&format!("TRIGGER:-P{lead}D")));
        }
    }

    #[test]
    fn a_deposition_exports_the_actual_start_and_its_buffered_busy_window() {
        let mut deposition = row();
        deposition["due_at"] = json!("2028-07-12T10:00:00-07:00");
        deposition["category"] = json!("deposition");
        deposition["hearing"] = json!({"duration_minutes":120,"location":"Remote"});
        let text = render(&calendar(), &[deposition]).unwrap();
        assert!(text.contains("20280712T100000"));
        assert!(text.contains("DTSTART:20280712T164500Z"));
        assert!(text.contains("DTEND:20280712T191500Z"));
    }

    #[test]
    fn a_court_date_awaiting_review_never_reaches_the_ics_feed() {
        let mut pending = row();
        pending["calendar_review_pending"] = json!(true);
        assert!(!render(&calendar(), &[pending])
            .unwrap()
            .contains("BEGIN:VEVENT"));
        assert!(render(&calendar(), &[row()])
            .unwrap()
            .contains("BEGIN:VEVENT"));
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
            "SUMMARY:acme: Court order deadline",
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
            "DESCRIPTION:Submit by 2026-11-02T16:45:00-08:00\\nAuthority: Order setting briefing\\, entered 2026-10-05\\nSource document: 0190a000-0000-7000-8000-0000000000aa\\nDeadline: 0190a000-0000-7000-8000-000000000001\\nMatter page: https://navigator.example.test/app/projects/acme"
        ));
    }

    #[test]
    fn a_tickler_uses_its_classified_title_and_an_unknown_legacy_category_keeps_its_title() {
        let mut tickler = row();
        tickler["category"] = json!("tickler");
        tickler["title"] = json!("Review response");
        assert!(render(&calendar(), &[tickler])
            .unwrap()
            .contains("SUMMARY:acme: TICKLER – Review response\r\n"));

        let mut legacy = row();
        legacy["category"] = json!("filing");
        assert_eq!(
            render(&calendar(), &[legacy]).unwrap(),
            render(&calendar(), &[row()]).unwrap()
        );
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
        assert!(text.contains("SUMMARY:acme: HEARING – Hearing\r\n"));
    }

    #[test]
    fn a_title_names_the_event_and_a_row_without_one_keeps_the_kind() {
        let mut titled = row();
        titled["title"] = json!("Answer to the complaint");
        let text = render(&calendar(), &[titled]).unwrap();
        assert!(text.contains("SUMMARY:acme: Answer to the complaint\r\n"));
        let mut untitled = row();
        untitled["title"] = json!(null);
        let text = render(&calendar(), &[untitled]).unwrap();
        assert!(text.contains("SUMMARY:acme: Court order deadline\r\n"));
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
