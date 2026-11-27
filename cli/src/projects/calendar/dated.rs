use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{json, Value};
use std::process::ExitCode;

use crate::remote;

use super::{super::deadline_brief, Instant};

pub(super) async fn run(
    host: Option<&str>,
    date: &str,
    zone: Option<&str>,
    json: bool,
) -> ExitCode {
    let result: Result<Value> = async {
        let zone = deadline_brief::local_zone(zone)?;
        let (day, start, end, now) = dates(date, zone, Utc::now())?;
        let (base, token) = remote::resolve(host)?;
        let (deadlines, calendar) = tokio::join!(
            deadline_brief::agenda_rows(&base, &token, day, now, zone),
            super::post(
                &base,
                &token,
                "/app/api/calendars/events",
                json!({"start": start, "end": end})
            ),
        );
        let mut report = assemble(day, start, end, now, deadlines, calendar);
        report["timezone"] = json!(zone.name());
        Ok(report)
    }
    .await;
    match result {
        Ok(report) => {
            if json {
                println!("{report}");
            } else {
                print(&report);
            }
            if report["complete"] == true {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(error) => {
            eprintln!("navigator calendar: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn dates(
    date: &str,
    zone: Tz,
    now: DateTime<Utc>,
) -> Result<(NaiveDate, Instant, Instant, DateTime<Utc>)> {
    let day = if date == "today" {
        now.with_timezone(&zone).date_naive()
    } else {
        date.parse().context("--date must be YYYY-MM-DD or today")?
    };
    let midnight = |date: NaiveDate| {
        zone.from_local_datetime(&date.and_time(NaiveTime::MIN))
            .earliest()
            .map(|time| time.fixed_offset())
            .context("agenda date has no local midnight in the selected time zone")
    };
    let next = day.succ_opt().context("agenda date is too late")?;
    Ok((day, midnight(day)?, midnight(next)?, now))
}

fn assemble(
    day: NaiveDate,
    start: Instant,
    end: Instant,
    now: DateTime<Utc>,
    deadlines: Result<Value>,
    calendar: Result<Value>,
) -> Value {
    let mut entries = Vec::new();
    let mut overdue = Vec::new();
    let mut schedule = Value::Null;
    let mut appearances = Value::Null;
    let mut deadline_rows = Value::Null;
    let deadline_coverage = match deadlines {
        Ok(mut report) => {
            schedule = std::mem::take(&mut report["work_schedule"]);
            appearances = std::mem::take(&mut report["appearances"]);
            deadline_rows = report["deadlines"].clone();
            if let Some(capacity) = schedule["capacity"].as_object_mut() {
                capacity.retain(|date, _| date == &day.to_string());
            }
            for row in report["deadlines"].as_array().into_iter().flatten() {
                let Some(due) = row["due_at"]
                    .as_str()
                    .and_then(|value| value.parse::<Instant>().ok())
                else {
                    continue;
                };
                let priority = row["at_risk"] == true
                    || matches!(
                        row["category"].as_str(),
                        Some("hearing" | "trial_day" | "deposition")
                    )
                    || row["calendar_role"] == "hearing"
                    || row["authority_kind"] == "court_order";
                let mut entry = row.clone();
                entry["kind"] = json!("deadline");
                let civil = row["due_on"]["date"]
                    .as_str()
                    .and_then(|value| value.parse::<NaiveDate>().ok());
                entry["at"] = civil.map_or_else(|| json!(due), |date| json!(date));
                entry["all_day"] = json!(civil.is_some());
                entry["priority"] = json!(priority);
                if due < now {
                    overdue.push(entry);
                } else if civil.map_or_else(|| due >= start && due < end, |date| date == day) {
                    entries.push((!priority, if civil.is_some() { start } else { due }, entry));
                }
            }
            json!({"complete": report["complete"], "projects": report["coverage"]})
        }
        Err(error) => json!({"complete": false, "error": error.to_string()}),
    };
    let calendar_coverage = match calendar {
        Ok(events) if events.is_array() => {
            let mut unreadable = 0;
            for event in events.as_array().into_iter().flatten() {
                let begins = &event["start"];
                let instant = begins["dateTime"]
                    .as_str()
                    .and_then(|value| value.parse::<Instant>().ok());
                let all_day = begins["date"]
                    .as_str()
                    .and_then(|value| value.parse::<NaiveDate>().ok())
                    .is_some();
                if let Some(at) = instant.or(all_day.then_some(start)) {
                    entries.push((true, at.max(start), json!({
                        "kind": "calendar_event", "at": at, "all_day": all_day, "priority": false,
                        "title": event["summary"].as_str().unwrap_or("Calendar event"),
                        "event": event,
                    })));
                } else {
                    unreadable += 1;
                }
            }
            json!({"complete": unreadable == 0, "unreadable_events": unreadable})
        }
        Ok(gap) => json!({"complete": false, "detail": gap}),
        Err(error) => json!({"complete": false, "error": error.to_string()}),
    };
    entries.sort_by_key(|(ordinary, at, entry)| (entry["at_risk"] != true, *ordinary, *at));
    json!({
        "date": day, "as_of": now,
        "complete": deadline_coverage["complete"] == true && calendar_coverage["complete"] == true,
        "coverage": {"deadlines": deadline_coverage, "calendar": calendar_coverage},
        "entries": entries.into_iter().map(|(_, _, entry)| entry).collect::<Vec<_>>(),
        "overdue": overdue,
        "work_schedule": schedule,
        "appearances": appearances,
        "deadlines": deadline_rows,
    })
}

fn print(report: &Value) {
    println!(
        "Agenda for {} ({})",
        report["date"].as_str().unwrap_or_default(),
        report["timezone"].as_str().unwrap_or_default()
    );
    let schedule = deadline_brief::scheduling_console(report);
    if !schedule.is_empty() {
        println!("{schedule}");
    }
    for (field, label) in [
        ("entries", "Scheduled and due"),
        ("overdue", "Overdue deadlines"),
    ] {
        println!("{label}");
        for row in report[field].as_array().into_iter().flatten() {
            let when = row["due_local"].as_str().unwrap_or_else(|| {
                if row["all_day"] == true {
                    "All day"
                } else {
                    row["at"].as_str().unwrap_or_default()
                }
            });
            let marker = if row["priority"] == true {
                "Court • "
            } else {
                ""
            };
            let project = row["project_code"]
                .as_str()
                .map_or(String::new(), |code| format!(" • {code}"));
            println!(
                "  {when} • {marker}{}{project}",
                row["title"].as_str().unwrap_or_default()
            );
            if let Some(at) = row["submit_by"].as_str() {
                println!("    Submit by {at}");
            }
            if let (Some(start), Some(end)) = (
                row["busy_window"][0].as_str(),
                row["busy_window"][1].as_str(),
            ) {
                println!("    Busy {start} – {end}");
            }
        }
    }
    if report["complete"] != true {
        println!("Incomplete coverage: {}", report["coverage"]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_local_day_tracks_dst_and_keeps_calendar_gaps_visible() {
        let (day, start, end, now) = dates(
            "today",
            chrono_tz::America::New_York,
            "2026-03-08T12:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!((end - start).num_hours(), 23);
        let report = assemble(
            day,
            start,
            end,
            now,
            Ok(json!({"complete": true, "deadlines": []})),
            Ok(json!({"unreadable": [{"missing": "scope"}]})),
        );
        assert_eq!(report["complete"], false);
        assert_eq!(
            report["coverage"]["calendar"]["detail"]["unreadable"][0]["missing"],
            "scope"
        );
    }

    #[test]
    fn the_agenda_lists_a_civil_deadline_only_on_its_recorded_date() {
        let now = "2028-07-12T12:00:00Z".parse().unwrap();
        let brief = json!({"complete":true, "deadlines":[{
            "due_on":{"date":"2028-07-12", "timezone":"America/Los_Angeles", "cutoff":"23:59:00"},
            "due_at":"2028-07-13T06:59:00Z", "title":"Synthetic response"
        }]});
        for (date, expected_count) in [("2028-07-12", 1), ("2028-07-13", 0)] {
            let (day, start, end, now) = dates(date, chrono_tz::America::New_York, now).unwrap();
            let report = assemble(day, start, end, now, Ok(brief.clone()), Ok(json!([])));
            let entries = report["entries"].as_array().unwrap();
            assert_eq!(entries.len(), expected_count);
            if let Some(entry) = entries.first() {
                assert_eq!(entry["all_day"], true);
                assert_eq!(entry["at"], "2028-07-12");
            }
        }
    }

    #[test]
    fn court_work_leads_the_day_and_overdue_work_stays_separate() {
        let (day, start, end, now) = dates(
            "2026-11-24",
            chrono_tz::America::New_York,
            "2026-11-24T15:00:00Z".parse().unwrap(),
        )
        .unwrap();
        assert_eq!(day.to_string(), "2026-11-24");
        let brief = json!({"complete": true, "deadlines": [
            {"due_at": "2026-11-24T12:00:00Z", "title": "Past internal"},
            {"due_at": "2026-11-24T17:00:00Z", "title": "Internal"},
            {"due_at": "2026-11-24T18:00:00Z", "title": "Hearing", "category": "hearing"},
            {"due_at": "2026-11-25T06:00:00Z", "title": "Tomorrow"}
        ]});
        let calendar =
            json!([{"summary": "Meeting", "start": {"dateTime": "2026-11-24T16:00:00Z"}}]);
        let report = assemble(day, start, end, now, Ok(brief), Ok(calendar));
        assert_eq!(report["entries"][0]["title"], "Hearing");
        assert_eq!(report["entries"][1]["title"], "Meeting");
        assert_eq!(report["entries"][2]["title"], "Internal");
        assert_eq!(report["overdue"][0]["title"], "Past internal");
        assert_eq!(report["entries"].as_array().unwrap().len(), 3);
    }
}
