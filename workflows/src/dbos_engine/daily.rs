//! The daily slot loop shared by DBOS ports of once-a-day Restate CronJobs.
//!
//! A port names its UTC time of day and a [`LateStart`] policy. The policy
//! decides what an executor that starts *after* today's slot does: run today's
//! slot now, or wait for tomorrow's. Waiting matters for a port whose Restate
//! CronJob may already have run today's slot before the deployment switched
//! engines, and for a port whose effect is wrong late in the day (a reset that
//! would delete drafts made since the slot). Slot-keyed workflow ids make
//! duplicate starts of the *same* slot join one run; the policy decides
//! whether a late executor starts that slot at all.

use std::future::Future;
use std::time::Duration;

use chrono::{DateTime, NaiveTime, TimeDelta, Utc};
use dbos::CancellationToken;

const RETRY_START: Duration = Duration::from_mins(1);

/// What an executor that starts after today's slot does with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LateStart {
    /// Start today's slot immediately, however late.
    CatchUp,
    /// Start today's slot only when no later than this past it; otherwise
    /// wait for tomorrow's. The window covers a restart or rolling update
    /// that lands on the slot instant.
    Within(Duration),
}

/// Today's slot at `at` UTC on the date of `now`.
#[must_use]
pub fn slot(now: DateTime<Utc>, at: NaiveTime) -> DateTime<Utc> {
    now.date_naive().and_time(at).and_utc()
}

/// The next slot this executor should start, given the clock and the policy.
/// Before today's slot it is today's; after it, today's under
/// [`LateStart::CatchUp`] or inside the [`LateStart::Within`] window, and
/// otherwise tomorrow's.
#[must_use]
pub fn next_slot(now: DateTime<Utc>, at: NaiveTime, late: LateStart) -> DateTime<Utc> {
    let today = slot(now, at);
    let late_by = now - today;
    let start_today = match late {
        _ if late_by <= TimeDelta::zero() => true,
        LateStart::CatchUp => true,
        LateStart::Within(window) => {
            TimeDelta::from_std(window).is_ok_and(|window| late_by <= window)
        }
    };
    if start_today {
        today
    } else {
        today + TimeDelta::days(1)
    }
}

/// Start each due slot with `fire`, sleeping in between, until `stop`.
///
/// After a slot starts, the next one is exactly a day later; a failed start
/// retries the same slot after a minute, so a retry is never "late".
pub async fn run<F, Fut, E>(
    workflow: &'static str,
    at: NaiveTime,
    late: LateStart,
    clock: impl Fn() -> DateTime<Utc>,
    stop: CancellationToken,
    fire: F,
) where
    F: Fn(DateTime<Utc>) -> Fut,
    Fut: Future<Output = Result<(), E>>,
    E: std::fmt::Display,
{
    let mut due = next_slot(clock(), at, late);
    loop {
        let now = clock();
        let wait = if now >= due {
            match fire(due).await {
                Ok(()) => {
                    due += TimeDelta::days(1);
                    (due - now).to_std().unwrap_or(Duration::ZERO)
                }
                Err(error) => {
                    tracing::error!(workflow, slot = %due, %error, "DBOS daily start failed");
                    RETRY_START
                }
            }
        } else {
            (due - now).to_std().unwrap_or(Duration::ZERO)
        };
        tokio::select! {
            () = stop.cancelled() => return,
            () = tokio::time::sleep(wait) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{next_slot, LateStart};
    use chrono::{DateTime, NaiveTime, TimeZone, Utc};
    use std::time::Duration;

    fn at(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
    }

    fn utc(day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, day, hour, minute, 0)
            .unwrap()
    }

    const WINDOW: LateStart = LateStart::Within(Duration::from_mins(15));

    #[test]
    fn before_the_slot_both_policies_wait_for_today() {
        for late in [LateStart::CatchUp, WINDOW] {
            assert_eq!(next_slot(utc(21, 4, 0), at(4, 20), late), utc(21, 4, 20));
            assert_eq!(next_slot(utc(21, 4, 20), at(4, 20), late), utc(21, 4, 20));
        }
    }

    #[test]
    fn catch_up_starts_today_however_late() {
        assert_eq!(
            next_slot(utc(21, 23, 59), at(4, 20), LateStart::CatchUp),
            utc(21, 4, 20)
        );
    }

    #[test]
    fn a_window_starts_today_only_inside_it() {
        assert_eq!(next_slot(utc(21, 4, 35), at(4, 20), WINDOW), utc(21, 4, 20));
        assert_eq!(next_slot(utc(21, 4, 36), at(4, 20), WINDOW), utc(22, 4, 20));
        assert_eq!(
            next_slot(utc(21, 13, 5), at(4, 20), WINDOW),
            utc(22, 4, 20),
            "a mid-day switch waits for tomorrow"
        );
    }

    #[tokio::test]
    async fn the_loop_waits_for_tomorrow_when_late_and_exits_on_stop() {
        let fired = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = dbos::CancellationToken::new();
        let recorded = std::sync::Arc::clone(&fired);
        let task = tokio::spawn({
            let stop = stop.clone();
            async move {
                super::run(
                    "Synthetic",
                    at(4, 20),
                    WINDOW,
                    || utc(21, 13, 0),
                    stop,
                    move |slot| {
                        recorded.lock().unwrap().push(slot);
                        async { Ok::<(), String>(()) }
                    },
                )
                .await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.cancel();
        task.await.unwrap();
        assert!(fired.lock().unwrap().is_empty(), "nothing fires mid-day");
    }

    #[tokio::test]
    async fn the_loop_fires_today_inside_the_window() {
        let fired = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let stop = dbos::CancellationToken::new();
        let recorded = std::sync::Arc::clone(&fired);
        let task = tokio::spawn({
            let stop = stop.clone();
            async move {
                super::run(
                    "Synthetic",
                    at(4, 20),
                    WINDOW,
                    || utc(21, 4, 25),
                    stop,
                    move |slot| {
                        recorded.lock().unwrap().push(slot);
                        async { Ok::<(), String>(()) }
                    },
                )
                .await;
            }
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        stop.cancel();
        task.await.unwrap();
        assert_eq!(*fired.lock().unwrap(), [utc(21, 4, 20)]);
    }
}
