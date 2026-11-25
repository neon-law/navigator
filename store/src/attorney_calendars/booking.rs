//! Project availability projected onto an attorney's private booking calendar.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, FixedOffset};
use cloud::calendar::{
    CalendarError, CalendarEvent, CalendarService, EventTiming, ProjectCalendarEvent,
    BOOKING_INTERVAL as INTERVAL, BOOKING_MARKER as MARKER, BOOKING_VERSION as VERSION,
};

type Time = DateTime<FixedOffset>;

fn intervals(events: Vec<ProjectCalendarEvent>) -> Result<Vec<(Time, Time)>, CalendarError> {
    let mut intervals = Vec::new();
    for event in events {
        if event.transparency.as_deref() == Some("transparent") || event.start.date.is_some() {
            continue;
        }
        let parse = |value: Option<String>| {
            value
                .and_then(|value| DateTime::parse_from_rfc3339(&value).ok())
                .map(|time| time.to_utc().fixed_offset())
                .ok_or(CalendarError::UnverifiedEvent)
        };
        let start = parse(event.start.date_time)?;
        let end = parse(event.end.date_time)?;
        if end <= start {
            return Err(CalendarError::UnverifiedEvent);
        }
        intervals.push((start, end));
    }
    intervals.sort_unstable();
    let mut merged: Vec<(Time, Time)> = Vec::new();
    for (start, end) in intervals {
        match merged.last_mut() {
            Some((_, previous_end)) if start <= *previous_end => {
                *previous_end = end.max(*previous_end);
            }
            _ => merged.push((start, end)),
        }
    }
    Ok(merged)
}

/// Add holds from verified Project events. Prune only from a complete snapshot.
/// The caller verifies calendar ownership and holds the attorney's Calendar write lane.
pub async fn reconcile(
    provider: &dyn CalendarService,
    target: &str,
    events: Vec<ProjectCalendarEvent>,
    complete: bool,
) -> Result<u64, CalendarError> {
    let desired = intervals(events)?;
    let count = desired.len() as u64;
    let stored = provider.mirrored_events(target, MARKER, VERSION).await?;
    if stored
        .iter()
        .any(|event| !event.can_write() || !event.private_properties.contains_key(INTERVAL))
    {
        return Err(CalendarError::UnverifiedEvent);
    }
    let mut keep = BTreeSet::new();
    for (start, end) in desired {
        let key = format!("{}/{}", start.to_rfc3339(), end.to_rfc3339());
        if let Some(event) = stored
            .iter()
            .find(|event| event.private_properties.get(INTERVAL) == Some(&key))
        {
            keep.insert(event.id.as_str());
            continue;
        }
        provider
            .insert_event(
                target,
                &CalendarEvent {
                    summary: "Busy".into(),
                    description: None,
                    location: None,
                    color_id: None,
                    start,
                    timing: EventTiming::BusyUntil(end),
                    sequence: 0,
                    cancelled: false,
                    reminder_minutes: Vec::new(),
                    private_properties: BTreeMap::from([
                        (MARKER.into(), VERSION.into()),
                        (INTERVAL.into(), key),
                    ]),
                },
            )
            .await?;
    }
    if complete {
        for event in &stored {
            if !keep.contains(event.id.as_str()) {
                provider.delete_event(target, event).await?;
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use cloud::calendar::FakeCalendar;

    fn source(start: &str, end: &str, transparency: &str) -> ProjectCalendarEvent {
        serde_json::from_value(serde_json::json!({
            "id": "source",
            "summary": "Matter details",
            "start": {"dateTime": start},
            "end": {"dateTime": end},
            "transparency": transparency
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn a_new_project_blocks_time_and_a_later_snapshot_restores_it() {
        let calendar = FakeCalendar::default();
        let target = calendar
            .create_calendar("Availability", "UTC")
            .await
            .unwrap()
            .id;
        let snapshot = || {
            vec![
                source("2026-11-26T12:00:00Z", "2026-11-26T13:00:00Z", "opaque"),
                source(
                    "2026-11-26T07:30:00-05:00",
                    "2026-11-26T09:10:20-05:00",
                    "opaque",
                ),
                source(
                    "2026-11-26T15:00:00Z",
                    "2026-11-26T16:00:00Z",
                    "transparent",
                ),
            ]
        };

        assert_eq!(
            reconcile(&calendar, &target, snapshot(), true)
                .await
                .unwrap(),
            1
        );
        let first = calendar
            .mirrored_events(&target, MARKER, VERSION)
            .await
            .unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(
            reconcile(&calendar, &target, snapshot(), true)
                .await
                .unwrap(),
            1
        );
        assert_eq!(
            calendar
                .mirrored_events(&target, MARKER, VERSION)
                .await
                .unwrap(),
            first
        );

        let events = calendar
            .events_in_window(
                &target,
                "2026-11-26T00:00:00Z".parse().unwrap(),
                "2026-11-27T00:00:00Z".parse().unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].summary.as_deref(), Some("Busy"));
        assert_eq!(
            events[0].end.date_time.as_deref(),
            Some("2026-11-26T14:10:20+00:00")
        );

        assert!(reconcile(
            &calendar,
            &target,
            vec![source("2026-11-26T12:00:00Z", "invalid", "opaque")],
            true,
        )
        .await
        .is_err());
        assert_eq!(
            calendar
                .mirrored_events(&target, MARKER, VERSION)
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            reconcile(&calendar, &target, Vec::new(), false)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            calendar
                .mirrored_events(&target, MARKER, VERSION)
                .await
                .unwrap(),
            first
        );
        assert_eq!(
            reconcile(&calendar, &target, Vec::new(), true)
                .await
                .unwrap(),
            0
        );
        assert!(calendar
            .mirrored_events(&target, MARKER, VERSION)
            .await
            .unwrap()
            .is_empty());
    }
}
