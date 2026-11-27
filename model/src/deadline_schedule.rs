//! Allocate buffered Deadline work within the attorney's free working hours.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, NaiveTime, TimeDelta, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Window {
    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    #[must_use]
    pub fn intersection(self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(Self { start, end })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Work {
    pub deadline: Uuid,
    pub window: Window,
    pub minutes: i32,
    pub buffer_minutes: i64,
}

impl Work {
    #[must_use]
    pub fn new(
        deadline: Uuid,
        target: DateTime<Utc>,
        now: DateTime<Utc>,
        lead_days: Option<i64>,
        minutes: i32,
        buffer_minutes: i64,
    ) -> Self {
        let lead = TimeDelta::try_days(lead_days.unwrap_or(7)).unwrap_or(TimeDelta::MAX);
        let start = target
            .checked_sub_signed(lead)
            .map_or(now, |from| from.max(now));
        Self {
            deadline,
            window: Window { start, end: target },
            minutes,
            buffer_minutes,
        }
    }
}

/// Weekday working hours, clipped to the planning horizon and the Firm's cutoff.
#[must_use]
pub fn working_hours(zone: chrono_tz::Tz, horizon: Window, close: NaiveTime) -> Vec<Window> {
    let start_hour = NaiveTime::from_hms_opt(9, 0, 0).unwrap_or(NaiveTime::MIN);
    let first = horizon.start.with_timezone(&zone).date_naive();
    let last = horizon.end.with_timezone(&zone).date_naive();
    first
        .iter_days()
        .take_while(|day| *day <= last)
        .filter(|day| day.weekday().number_from_monday() <= 5)
        .filter_map(|day| {
            let start = zone
                .from_local_datetime(&day.and_time(start_hour))
                .earliest()?
                .to_utc();
            let end = zone
                .from_local_datetime(&day.and_time(close))
                .earliest()?
                .to_utc();
            (Window { start, end }).intersection(horizon)
        })
        .collect()
}

#[derive(Default, Debug, Serialize)]
pub struct WorkSchedule {
    pub blocks: BTreeMap<Uuid, Vec<Window>>,
    pub at_risk: BTreeMap<Uuid, Vec<Uuid>>,
}

fn subtract(slots: &mut Vec<Window>, occupied: Window) {
    let mut remaining = Vec::new();
    for slot in slots.drain(..) {
        let Some(overlap) = slot.intersection(occupied) else {
            remaining.push(slot);
            continue;
        };
        if slot.start < overlap.start {
            remaining.push(Window {
                start: slot.start,
                end: overlap.start,
            });
        }
        if overlap.end < slot.end {
            remaining.push(Window {
                start: overlap.end,
                end: slot.end,
            });
        }
    }
    *slots = remaining;
}

/// Work is placed in earliest-due order. An over-capacity window flags every competing Deadline.
#[must_use]
pub fn allocate(mut work: Vec<Work>, working_hours: &[Window], busy: &[Window]) -> WorkSchedule {
    work.retain(|task| task.minutes > 0);
    work.sort_unstable_by_key(|task| (task.window.end, task.deadline));
    let mut free = working_hours.to_vec();
    free.sort_unstable_by_key(|window| window.start);
    for block in busy {
        subtract(&mut free, *block);
    }
    let mut schedule = WorkSchedule::default();
    for task in &work {
        let mut remaining = i64::from(task.minutes);
        let overhead = task.buffer_minutes * 2;
        let mut blocks = Vec::new();
        for slot in free
            .iter()
            .filter_map(|slot| slot.intersection(task.window))
        {
            let minutes = remaining.min((slot.end - slot.start).num_minutes() - overhead);
            if minutes <= 0 {
                continue;
            }
            blocks.push(Window {
                start: slot.start,
                end: slot.start + TimeDelta::minutes(minutes + overhead),
            });
            remaining -= minutes;
            if remaining == 0 {
                break;
            }
        }
        for block in &blocks {
            subtract(&mut free, *block);
        }
        if !blocks.is_empty() {
            schedule.blocks.insert(task.deadline, blocks);
        }
        if remaining > 0 {
            for contender in work.iter().filter(|other| {
                other.window.overlaps(task.window) || other.deadline == task.deadline
            }) {
                let competing = schedule.at_risk.entry(contender.deadline).or_default();
                competing.extend(
                    work.iter()
                        .filter(|other| {
                            other.deadline != contender.deadline
                                && other.window.overlaps(contender.window)
                        })
                        .map(|other| other.deadline),
                );
                competing.sort_unstable();
                competing.dedup();
            }
        }
    }
    schedule
}

/// Working time used by any one of the Busy windows, counted once.
#[must_use]
pub fn capacity(working_hours: &[Window], busy: &[Window]) -> (i64, i64) {
    let minutes = |windows: &[Window]| {
        windows
            .iter()
            .map(|window| (window.end - window.start).num_minutes())
            .sum::<i64>()
    };
    let mut free = working_hours.to_vec();
    for block in busy {
        subtract(&mut free, *block);
    }
    let total = minutes(working_hours);
    (total - minutes(&free), total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_reserves_buffered_time_and_overcapacity_names_all_competitors() {
        let start = "2028-07-12T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let window = Window {
            start,
            end: start + TimeDelta::hours(8),
        };
        let tasks: Vec<_> = (1..=3)
            .map(|id| Work {
                deadline: Uuid::from_u128(id),
                window,
                minutes: 360,
                buffer_minutes: 15,
            })
            .collect();
        let first = allocate(tasks[..1].to_vec(), &[window], &[]);
        assert!(first.at_risk.is_empty());
        assert_eq!(
            first.blocks[&tasks[0].deadline][0].end,
            start + TimeDelta::minutes(390)
        );
        let full = allocate(tasks.clone(), &[window], &[]);
        for task in tasks {
            assert_eq!(full.at_risk[&task.deadline].len(), 2);
        }
    }
}
