//! Time scales: civil UTC into TDB seconds past J2000, the SPK time argument.
//!
//! The chain is UTC→TAI→TT→TDB, all through `hifitime`:
//!
//! 1. **UTC→TAI** — hifitime's built-in leap-second table (27 seconds as of
//!    2017, the table's last entry). For dates before 1972 hifitime models the
//!    pre-leap-second UTC drift, so 1900+ birth data converts on the same code
//!    path as modern dates.
//! 2. **TAI→TT** — the fixed 32.184 s offset of Terrestrial Time.
//! 3. **TT→TDB** — the periodic relativistic term (amplitude ≈ 1.6 ms) that
//!    converts the geoid clock to the solar-system barycenter clock the
//!    ephemeris integrator reports.
//!
//! The classical ΔT = TT−UT1 is **not** consumed here: SPK segments are
//! functions of TDB only. ΔT belongs to Earth-rotation quantities (the
//! ascendant and midheaven), which later layers compute.

use hifitime::Epoch;

/// TDB seconds past the J2000 epoch — the independent variable of SPK segments.
///
/// The value is opaque to interpolation; the SPK descriptors carry the same
/// quantity, so subtraction is the only arithmetic the reader performs.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TdbSeconds(pub f64);

/// Converts a UTC instant into [`TdbSeconds`].
#[must_use = "the converted TDB reading is returned"]
pub fn utc_to_tdb(utc: Epoch) -> TdbSeconds {
    TdbSeconds(utc.to_tdb_seconds())
}

/// Converts a civil UTC date and time into [`TdbSeconds`].
///
/// Panics-free alternative for parse-time validation of user input; returns
/// the hifitime error for invalid calendar combinations.
///
/// # Errors
///
/// Returns the underlying [`hifitime::HifitimeError`] for an impossible
/// Gregorian date (month 13, February 30, and so on).
pub fn utc_to_tdb_from_parts(
    year: i32,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
) -> Result<TdbSeconds, hifitime::HifitimeError> {
    let utc = Epoch::maybe_from_gregorian_utc(year, month, day, hour, minute, second, 0)?;
    Ok(TdbSeconds(utc.to_tdb_seconds()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The J2000 epoch is 2000-01-01 12:00:00 TDB. TT runs 64.184 s ahead of
    /// UTC (32 leap seconds + the fixed 32.184 s TAI→TT offset), so the same
    /// instant reads 11:58:55.816 on a civil UTC clock; it must convert to 0.
    #[test]
    fn j2000_instant_converts_to_zero() {
        let utc = Epoch::maybe_from_gregorian_utc(2000, 1, 1, 11, 58, 55, 816_000_000)
            .expect("valid J2000 civil date");
        let tdb = utc_to_tdb(utc);
        // The TT→TDB periodic term is bounded by ≈1.6 ms.
        assert!(
            tdb.0.abs() < 0.002,
            "J2000 epoch must convert to ≈0 TDB seconds, got {}",
            tdb.0
        );
    }

    /// A 2017 date converts through the current 37-second leap-second count.
    /// The meaningful check is the round-trip through `from_tdb_seconds`, and
    /// that the conversion lands TDB past the naive UTC reading of the same
    /// civil time by the expected 69.184 s offset (37 + 32.184) plus the µs
    /// periodic term.
    #[test]
    fn modern_date_applies_current_leap_seconds() {
        let tdb = utc_to_tdb_from_parts(2017, 1, 14, 0, 31, 55).expect("valid date");
        // TDB seconds run from J2000 in TDB; hifitime's UTC-to-UTC duration is
        // leap-free wall arithmetic. Their difference is therefore the full
        // TDB−UTC offset at the date: 37 leap seconds accumulated since J2000
        // (32 at the epoch) + the fixed 32.184 s TAI→TT offset, plus the
        // sub-millisecond TDB−TT periodic term.
        let utc_epoch = Epoch::from_gregorian_utc_hms(2017, 1, 14, 0, 31, 55);
        let utc_wall_elapsed =
            (utc_epoch - Epoch::from_gregorian_utc_hms(2000, 1, 1, 12, 0, 0)).to_seconds();
        let delta = tdb.0 - utc_wall_elapsed - 69.184;
        assert!(
            delta.abs() < 0.01,
            "expected TDB−UTC = 69.184 s (+ periodic ≤ 0.01 s), got {delta}"
        );
        let restored = Epoch::from_tdb_seconds(tdb.0);
        let back = utc_to_tdb(restored);
        assert!(
            (back.0 - tdb.0).abs() < 1e-6,
            "round trip drifted by {}",
            back.0 - tdb.0
        );
    }

    /// 1900 birth data converts on the pre-1972 hifitime path and stays finite
    /// and negative (well before J2000).
    #[test]
    fn pre_1972_date_converts() {
        let tdb = utc_to_tdb_from_parts(1900, 6, 15, 12, 0, 0).expect("valid historical date");
        // ~99.5 years before J2000 ≈ -3.14e9 s.
        assert!(
            (-3.2e9..=-3.0e9).contains(&tdb.0),
            "1900 epoch out of range: {}",
            tdb.0
        );
    }

    #[test]
    fn invalid_calendar_date_is_rejected() {
        assert!(utc_to_tdb_from_parts(2026, 2, 30, 0, 0, 0).is_err());
    }
}
