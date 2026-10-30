//! Greenwich apparent sidereal time — the Earth-rotation angle the line
//! engine converts into map longitudes.
//!
//! The chain (IERS Conventions 2010, chapter 6):
//!
//! 1. UT1 = TT − ΔT (the classical ΔT from [`deltat`]).
//! 2. ERA = 2π · fract(0.779 057 273 2640 + 1.002 737 811 911 354 48 ·
//!    (`JD_UT1` − 2451545.0)) — Earth Rotation Angle from UT1, exact by
//!    definition.
//! 3. GMST = ERA + the IAU 2006 polynomial (Capitaine, Wallace & Chapront
//!    2005), evaluated in TT centuries.
//! 4. GAST = GMST + dψ·cos(ε) — the equation of the equinoxes, with dψ the
//!    nutation in longitude (IAU 2000B, the same series the apparent-position
//!    frame uses) and ε the true obliquity.
//!
//! The ~0.2″ difference between IAU 2000B and the reference's nutation moves
//! the GAST by ≈0.00006° of longitude — three orders below the 0.01° line
//! validation bar.

use super::deltat::delta_t_seconds;
use crate::apparent::frame;
use crate::time::{utc_to_tdb, TdbSeconds};
use hifitime::Epoch;

/// Julian day of the J2000 epoch (2000 January 1.5, TT).
const J2000_JD: f64 = 2_451_545.0;

/// Days per Julian century, for the TT centuries argument.
const DAYS_PER_CENTURY: f64 = 36_525.0;

/// Seconds per day.
const SECONDS_PER_DAY: f64 = 86_400.0;

/// Greenwich apparent sidereal time in degrees, \[0, 360), at the civil
/// instant given as a UTC `Epoch`.
#[must_use]
pub fn gast_degrees(utc: Epoch) -> f64 {
    let TdbSeconds(tdb_seconds) = utc_to_tdb(utc);

    // TDB differs from TT by the ≤1.7 ms periodic relativistic term —
    // irrelevant at the precision of a Julian date here.
    let tt_days = J2000_JD + tdb_seconds / SECONDS_PER_DAY;
    // ΔT itself is tabulated against UTC civil time, so it is evaluated at
    // the UTC instant; UT1 then follows from TT − ΔT.
    let ut1_days =
        tt_days - delta_t_seconds(utc.to_jde_utc_days()) / SECONDS_PER_DAY;

    let era_deg = earth_rotation_angle_degrees(ut1_days);
    let gmst_poly_deg = gmst06_polynomial_degrees((tt_days - J2000_JD) / DAYS_PER_CENTURY);
    let equation_of_equinoxes_deg = equation_of_equinoxes_degrees(TdbSeconds(tdb_seconds));

    (era_deg + gmst_poly_deg + equation_of_equinoxes_deg).rem_euclid(360.0)
}

/// The Earth Rotation Angle in degrees from a UT1 Julian date.
fn earth_rotation_angle_degrees(ut1_days: f64) -> f64 {
    let days = ut1_days - J2000_JD;
    // IERS 2010 eq. 5.1; the constant is a fraction of a full turn.
    360.0 * (0.779_057_273_264_0 + 1.002_737_811_911_354_6 * days).rem_euclid(1.0)
}

/// The IAU 2006 GMST polynomial (arcseconds converted to degrees), in TT
/// centuries since J2000. Published as Capitaine, Wallace & Chapront (2005),
/// adopted in the IERS Conventions 2010 eq. 6.7.
fn gmst06_polynomial_degrees(centuries_tt: f64) -> f64 {
    let t = centuries_tt;
    let arcseconds = 0.014_506
        + 4_612.156_534 * t
        + 1.391_581_7 * t * t
        - 0.000_000_44 * t * t * t
        - 0.000_029_956 * t * t * t * t
        - 0.000_000_036_8 * t * t * t * t * t;
    arcseconds / 3600.0
}

/// The equation of the equinoxes: dψ·cos(ε), in degrees.
fn equation_of_equinoxes_degrees(tdb: TdbSeconds) -> f64 {
    let centuries = tdb.0 / (DAYS_PER_CENTURY * SECONDS_PER_DAY);
    let (dpsi, _) = frame::nut00b(centuries);
    let obliquity = frame::true_obliquity(tdb);
    (dpsi * obliquity.cos()).to_degrees()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn epoch_utc(year: i32, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Epoch {
        Epoch::maybe_from_gregorian_utc(year, month, day, hour, minute, second, 0)
            .expect("valid civil test date")
    }

    /// Seconds in a day, for converting ΔT and TT−UTC into days.
    const SECONDS_PER_DAY: f64 = 86_400.0;

    /// The composite sidereal rotation rate of the Earth, degrees per day of
    /// UT1 (ERA rate plus the IAU 2006 polynomial's linear term).
    const SIDEREAL_RATE_DEG_PER_DAY: f64 = 360.985_647_365;

    /// Swiss Ephemeris 2.10.03 `swe_sidtime` at 2000-01-01 12:00 UT returned
    /// 280.457072443° (`gen_probe` C harness, 2026-10-30). `swe_sidtime` treats
    /// its argument as UT1, while this module refines UT1 = TT − ΔT (here
    /// UT1−UTC = +0.356 s), rotating GAST by the sidereal rate times ΔUT1.
    /// The remaining model differences (IAU 2006 GMST vs Swiss Ephemeris's
    /// 1982 expression, IAU 2000B vs 1980 nutation) are under 1″.
    #[test]
    fn gast_matches_swiss_ephemeris_anchor() {
        let utc = epoch_utc(2000, 1, 1, 12, 0, 0);
        let gast = gast_degrees(utc);
        let se = 280.457_072_443;
        // TT−UTC at this instant: 32 leap seconds + 32.184 s.
        let tt_minus_utc = 64.184;
        let delta_t = delta_t_seconds(utc.to_jde_utc_days());
        let dut1_days = (tt_minus_utc - delta_t) / SECONDS_PER_DAY;
        let expected = se + SIDEREAL_RATE_DEG_PER_DAY * dut1_days;
        assert!(
            (gast - expected).abs() < 0.0002,
            "GAST {gast} vs adjusted Swiss Ephemeris anchor {expected}"
        );
    }

    /// Meeus, *Astronomical Algorithms* (2nd ed.), example 12.b: at
    /// 1987 April 10, 0h UT the Greenwich mean sidereal time is
    /// 13h 10m 46.3668s = 197.693195°. Meeus works on UT directly; this
    /// module's GMST is evaluated at UT1 = TT − ΔT (here UT1−UTC =
    /// −0.27 s), so the expected value carries that rotation.
    #[test]
    fn gmst_matches_meeus_example() {
        let utc = epoch_utc(1987, 4, 10, 0, 0, 0);
        let jd_utc = utc.to_jde_utc_days();
        let delta_t = delta_t_seconds(jd_utc);
        // TT−UTC on this date: 23 leap seconds + 32.184 s.
        let tt_minus_utc = 55.184;
        let dut1_days = (tt_minus_utc - delta_t) / SECONDS_PER_DAY;
        let expected = 197.693_195 + SIDEREAL_RATE_DEG_PER_DAY * dut1_days;
        let gast = gast_degrees(utc) - equation_of_equinoxes_degrees(utc_to_tdb(utc));
        assert!(
            (gast - expected).abs() < 0.001,
            "GMST {gast} vs adjusted Meeus example {expected}"
        );
    }

    /// ERA advances ≈360.9856° per day; one hour of UT must move it by
    /// ≈15.0411°.
    #[test]
    fn era_rate_matches_earth_rotation() {
        let a = earth_rotation_angle_degrees(J2000_JD);
        let b = earth_rotation_angle_degrees(J2000_JD + 1.0 / 24.0);
        let delta = (b - a + 360.0) % 360.0;
        assert!((delta - 15.041_1).abs() < 0.001, "hourly ERA step {delta}");
    }

    /// GAST stays wrapped into [0, 360) across a day of samples.
    #[test]
    fn gast_stays_wrapped() {
        for hour in 0..24 {
            let utc = epoch_utc(2026, 3, 20, hour, 0, 0);
            let gast = gast_degrees(utc);
            assert!((0.0..360.0).contains(&gast), "hour {hour}: {gast}");
        }
    }
}
