//! Civil birth-time resolution with explicit ambiguity and uncertainty.

use chrono::{Datelike, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::Serialize;
use std::num::NonZeroU32;

/// A supplied wall clock, IANA zone, and optional explicit UTC-offset choice.
#[derive(Clone)]
pub struct LocalBirthTime {
    /// Original civil date and time, without an inferred offset.
    pub local: NaiveDateTime,
    /// IANA time zone used to interpret the wall clock.
    pub zone: Tz,
    /// Explicit offset in seconds east of UTC; required for a repeated clock.
    pub offset_seconds: Option<i32>,
}

/// The supplied time and its precision. Personal inputs have no debug output.
#[derive(Clone)]
pub enum BirthTime {
    /// A precise wall clock whose universal reference must be resolved explicitly.
    Exact(LocalBirthTime),
    /// An approximate wall clock, with a positive uncertainty on either side.
    Approximate {
        /// Original civil clock and zone.
        local: LocalBirthTime,
        /// Uncertainty in seconds before and after the supplied time.
        uncertainty_seconds: NonZeroU32,
    },
    /// A date and zone with no supplied clock time.
    Unknown {
        /// Original civil date.
        date: NaiveDate,
        /// Original IANA time zone.
        zone: Tz,
    },
}

/// Time scale of the universal civil calendar label after timezone normalization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BirthTimeScale {
    /// Greenwich mean solar time; the historical civil-time convention before 1972.
    Ut1,
    /// Atomic civil time with the pinned leap-second table, from 1972 onward.
    Utc,
}

/// A normalized civil label and its explicit scale. Personal values have no debug output.
#[derive(Clone, Copy, Serialize)]
pub struct BirthTimeReference {
    /// Universal Gregorian calendar label, without a misleading UTC suffix.
    pub calendar: NaiveDateTime,
    /// Historical UT1 or modern UTC interpretation.
    pub scale: BirthTimeScale,
}

/// A validated resolution retaining the original input and its uncertainty.
#[derive(Clone)]
pub struct ResolvedBirthTime {
    original: BirthTime,
    reference: Option<BirthTimeReference>,
    offset_seconds: Option<i32>,
}

/// Time-resolution errors contain no personal input values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BirthTimeError {
    /// A repeated clock needs an explicit offset selection.
    #[error("civil time is ambiguous; select one of the zone's UTC offsets")]
    AmbiguousCivilTime,
    /// No instant has that wall clock in the selected zone.
    #[error("civil time does not identify a valid instant in this zone")]
    NonexistentCivilTime,
    /// The supplied offset is not one the zone uses at that civil time.
    #[error("selected UTC offset does not match the civil time and zone")]
    OffsetMismatch,
    /// Civil leap-second conversion requires separately verified semantics.
    #[error("civil leap-second input is not supported")]
    UnsupportedLeapSecond,
}

impl BirthTime {
    /// Resolve through pinned IANA rules without choosing a fold or shifting a gap.
    /// Unknown times retain their date and zone without an evaluation instant.
    pub fn resolve(&self) -> Result<ResolvedBirthTime, BirthTimeError> {
        let local = match self {
            Self::Exact(local) | Self::Approximate { local, .. } => local,
            Self::Unknown { .. } => {
                return Ok(ResolvedBirthTime {
                    original: self.clone(),
                    reference: None,
                    offset_seconds: None,
                });
            }
        };
        if local.local.nanosecond() >= 1_000_000_000 {
            return Err(BirthTimeError::UnsupportedLeapSecond);
        }
        let resolved = match local.zone.from_local_datetime(&local.local) {
            LocalResult::Single(instant) => instant,
            LocalResult::None => return Err(BirthTimeError::NonexistentCivilTime),
            LocalResult::Ambiguous(first, second) => {
                let offset = local
                    .offset_seconds
                    .ok_or(BirthTimeError::AmbiguousCivilTime)?;
                if first.offset().fix().local_minus_utc() == offset {
                    first
                } else if second.offset().fix().local_minus_utc() == offset {
                    second
                } else {
                    return Err(BirthTimeError::OffsetMismatch);
                }
            }
        };
        let offset = resolved.offset().fix().local_minus_utc();
        if local
            .offset_seconds
            .is_some_and(|requested| requested != offset)
        {
            return Err(BirthTimeError::OffsetMismatch);
        }
        Ok(ResolvedBirthTime {
            original: self.clone(),
            reference: Some(BirthTimeReference {
                calendar: resolved.naive_utc(),
                scale: if resolved.with_timezone(&Utc).year() < 1972 {
                    BirthTimeScale::Ut1
                } else {
                    BirthTimeScale::Utc
                },
            }),
            offset_seconds: Some(offset),
        })
    }
}

impl ResolvedBirthTime {
    /// Original civil input, including its zone, offset choice, and precision.
    #[must_use]
    pub fn original(&self) -> &BirthTime {
        &self.original
    }

    /// Universal civil label and scale; absent for an unknown clock.
    #[must_use]
    pub fn reference(&self) -> Option<BirthTimeReference> {
        self.reference
    }

    /// Physical evaluation epoch and explicit UT1, absent for an unknown clock.
    pub fn instant(&self) -> Result<Option<crate::ChartInstant>, crate::ChartError> {
        self.reference
            .map(crate::ChartInstant::from_reference)
            .transpose()
    }

    /// Resolved offset, in seconds east of UTC; absent for unknown time.
    #[must_use]
    pub fn offset_seconds(&self) -> Option<i32> {
        self.offset_seconds
    }

    /// Precision-dependent angles and houses require an exact resolved clock.
    #[must_use]
    pub fn can_compute_angles(&self) -> bool {
        matches!(&self.original, BirthTime::Exact(_))
    }

    /// The pinned IANA rules used by this calculation's time resolution.
    #[must_use]
    pub fn tzdb_version(&self) -> &'static str {
        chrono_tz::IANA_TZDB_VERSION
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveDateTime, Timelike};
    use std::num::NonZeroU32;

    fn local(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> LocalBirthTime {
        LocalBirthTime {
            local: NaiveDate::from_ymd_opt(year, month, day)
                .and_then(|date| date.and_hms_opt(hour, minute, 0))
                .expect("valid synthetic civil time"),
            zone: chrono_tz::America::New_York,
            offset_seconds: None,
        }
    }

    #[test]
    fn scale_boundary_uses_the_normalized_universal_calendar() {
        for (hour, minute, scale) in [(18, 59, BirthTimeScale::Ut1), (19, 0, BirthTimeScale::Utc)] {
            let resolved = BirthTime::Exact(local(1971, 12, 31, hour, minute))
                .resolve()
                .expect("synthetic boundary clock");
            let reference = resolved.reference().expect("known clock");
            assert_eq!(reference.scale, scale);
            assert_eq!(reference.calendar.hour(), if hour == 18 { 23 } else { 0 });
        }
    }

    #[test]
    fn a_fold_requires_an_explicit_offset() {
        assert!(
            matches!(
                BirthTime::Exact(local(2007, 11, 4, 1, 30)).resolve(),
                Err(BirthTimeError::AmbiguousCivilTime)
            ),
            "a repeated wall clock must not choose an instant implicitly"
        );
    }

    #[test]
    fn both_fold_offsets_select_distinct_instants_without_changing_the_wall_clock() {
        for (offset, hour) in [(-14_400, 5), (-18_000, 6)] {
            let mut input = local(2007, 11, 4, 1, 30);
            input.offset_seconds = Some(offset);
            let original = input.local;
            let resolved = BirthTime::Exact(input)
                .resolve()
                .expect("explicit fold offset");
            assert!(
                resolved
                    .reference()
                    .is_some_and(|reference| reference.calendar.hour() == hour
                        && reference.calendar.minute() == 30),
                "the selected fold offset must identify its UTC instant"
            );
            assert!(
                resolved.offset_seconds() == Some(offset),
                "the resolved offset must be retained"
            );
            assert!(
                matches!(resolved.original(), BirthTime::Exact(value) if value.local == original),
                "the original wall clock must be retained"
            );
            assert!(
                resolved.can_compute_angles(),
                "an exact disambiguated instant permits angles"
            );
        }
    }

    #[test]
    fn a_gap_is_rejected_without_shifting_the_time() {
        assert!(
            matches!(
                BirthTime::Exact(local(2007, 3, 11, 2, 30)).resolve(),
                Err(BirthTimeError::NonexistentCivilTime)
            ),
            "a skipped wall clock must not become a different wall clock"
        );
    }

    #[test]
    fn an_offset_must_match_the_zone_at_that_civil_time() {
        let mut input = local(2007, 11, 4, 1, 30);
        input.offset_seconds = Some(0);
        assert!(
            matches!(
                BirthTime::Exact(input).resolve(),
                Err(BirthTimeError::OffsetMismatch)
            ),
            "an arbitrary offset must not bypass the IANA zone"
        );
    }

    #[test]
    fn an_unknown_time_preserves_the_date_and_zone_without_a_noon_instant() {
        let date = NaiveDate::from_ymd_opt(2007, 3, 11).expect("valid synthetic date");
        let resolved = BirthTime::Unknown {
            date,
            zone: chrono_tz::America::New_York,
        }
        .resolve()
        .expect("unknown time is representable");
        assert!(
            resolved.reference().is_none(),
            "an unknown time has no exact UTC instant"
        );
        assert!(
            resolved.offset_seconds().is_none(),
            "an unknown time has no invented offset"
        );
        assert!(
            !resolved.can_compute_angles(),
            "unknown time disables precision-dependent angles"
        );
        assert!(
            matches!(resolved.original(), BirthTime::Unknown { date: original, zone }
            if *original == date && *zone == chrono_tz::America::New_York),
            "date and zone must survive resolution"
        );
    }

    #[test]
    fn approximate_time_preserves_uncertainty_and_disables_angles() {
        let uncertainty_seconds = NonZeroU32::new(600).expect("positive uncertainty");
        let resolved = BirthTime::Approximate {
            local: local(2000, 1, 1, 12, 0),
            uncertainty_seconds,
        }
        .resolve()
        .expect("valid approximate clock");
        assert!(
            resolved.reference().is_some(),
            "approximate positions can identify their evaluation instant"
        );
        assert!(
            !resolved.can_compute_angles(),
            "approximate time disables precision-dependent angles"
        );
        assert!(
            matches!(resolved.original(), BirthTime::Approximate { uncertainty_seconds: value, .. }
            if *value == uncertainty_seconds),
            "uncertainty must be retained"
        );
    }

    #[test]
    fn an_unverified_leap_second_is_rejected() {
        let mut input = local(2016, 12, 31, 23, 59);
        input.zone = chrono_tz::UTC;
        input.local = NaiveDateTime::new(
            input.local.date(),
            chrono::NaiveTime::from_hms_nano_opt(23, 59, 59, 1_000_000_000)
                .expect("chrono leap encoding"),
        );
        assert!(
            matches!(
                BirthTime::Exact(input).resolve(),
                Err(BirthTimeError::UnsupportedLeapSecond)
            ),
            "a leap-second civil clock needs explicit supported conversion semantics"
        );
    }
}
