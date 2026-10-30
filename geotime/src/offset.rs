//! UTC-offset resolution on a historical date, with local-mean-time fallback.
//!
//! `chrono-tz` carries the full IANA tzdata history, so a date's offset
//! reflects the rules actually in force then — wartime and pre-1966 local
//! daylight saving, zone recreations, and so on. For births before a zone's
//! first transition to standard time, tzdata reports the zone's local mean
//! time; [`offset_for`] flags that case so callers can warn about reduced
//! accuracy.

use chrono::{
    FixedOffset, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, Offset, TimeZone, Utc,
};
use chrono_tz::Tz;

/// How the UTC offset for a birth date was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffsetBasis {
    /// The IANA zone's own rules cover this date.
    ZoneRule,
    /// The date predates the zone's first transition to standard time: the
    /// offset is local mean time at the zone's reference location, and line
    /// placement carries that uncertainty.
    LocalMeanTime,
}

/// The offset a zone reports before any standard-time adoption — the IANA
/// "LMT" entry. 1800-01-01 predates every standard-time adoption in tzdata,
/// so whatever the zone answers there is its mean-time offset.
fn mean_time_offset(zone: Tz) -> FixedOffset {
    let probe = Utc
        .with_ymd_and_hms(1800, 1, 1, 12, 0, 0)
        .single()
        .expect("invariant: 1800-01-01T12:00Z is a single unambiguous UTC instant");
    zone.offset_from_utc_datetime(&probe.naive_utc()).fix()
}

/// The UTC offset in effect at `date` and local wall-clock `time`, plus the
/// basis it was determined on.
///
/// A wall clock that repeats (DST fall-back) resolves to the earlier instant;
/// a wall clock that never happens (a spring-forward gap — historically up to
/// a whole day, e.g. Samoa's 2011 date-line change) steps forward in
/// 30-minute increments until it does.
#[must_use]
pub fn offset_for(zone: Tz, date: NaiveDate, time: NaiveTime) -> (FixedOffset, OffsetBasis) {
    let naive = NaiveDateTime::new(date, time);
    let resolved = match zone.from_local_datetime(&naive) {
        LocalResult::Single(when) | LocalResult::Ambiguous(when, _) => when,
        LocalResult::None => resolve_gap(zone, naive),
    };
    let offset = resolved.offset().fix();
    let basis = if offset == mean_time_offset(zone) {
        OffsetBasis::LocalMeanTime
    } else {
        OffsetBasis::ZoneRule
    };
    (offset, basis)
}

/// The first instant at or after a skipped wall clock that names a real
/// local time. `naive` itself lands in a DST gap, so step the wall clock
/// forward until `from_local_datetime` resolves.
fn resolve_gap(zone: Tz, naive: NaiveDateTime) -> chrono::DateTime<Tz> {
    const STEP_MINUTES: i64 = 30;
    const MAX_STEPS: i64 = 96; // 48 hours — covers every recorded gap.
    (1..=MAX_STEPS)
        .find_map(|step| {
            zone.from_local_datetime(&(naive + chrono::Duration::minutes(STEP_MINUTES * step)))
                .single()
        })
        .unwrap_or_else(|| {
            // Unreachable for tzdata's gaps; interpret the wall clock as UTC
            // rather than give up on the birth record.
            zone.from_utc_datetime(&naive)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_local_mean_time_before_standard_time() {
        let date = NaiveDate::from_ymd_opt(1880, 5, 5).expect("invariant: valid date");
        let time = NaiveTime::from_hms_opt(10, 30, 0).expect("invariant: valid time");
        let (offset, basis) = offset_for(chrono_tz::America::New_York, date, time);
        // New York's LMT is UTC-4:56:02.
        assert_eq!(
            offset,
            FixedOffset::east_opt(-17_762).expect("invariant: valid offset")
        );
        assert_eq!(basis, OffsetBasis::LocalMeanTime);
    }

    #[test]
    fn steps_through_a_spring_forward_gap() {
        // 2007-03-11 02:30 does not exist in New York; the first real wall
        // clock at or after it is 03:00 EDT, UTC-4.
        let date = NaiveDate::from_ymd_opt(2007, 3, 11).expect("invariant: valid date");
        let time = NaiveTime::from_hms_opt(2, 30, 0).expect("invariant: valid time");
        let (offset, basis) = offset_for(chrono_tz::America::New_York, date, time);
        assert_eq!(
            offset,
            FixedOffset::east_opt(-14_400).expect("invariant: valid offset")
        );
        assert_eq!(basis, OffsetBasis::ZoneRule);
    }
}
