//! Delta-T (TT minus UT1) from pinned primary observations and explicit forecasts.
//!
//! Before 1955: Espenak/Meeus historical polynomials. From 1955: linear
//! interpolation of USNO historic and monthly observations, followed by the
//! published USNO forecast. Beyond its last sample, the terminal line segment
//! is extended and marked Extrapolation with no reported error bound.
//! Source bytes and their identities are verified and embedded at build time.

use serde::Serialize;

/// The evidence supporting the Earth-rotation correction at an instant.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeltaTKind {
    /// Published historical polynomial, without a stated error for this instant.
    HistoricalModel,
    /// Interpolated or exact tabulated USNO observation.
    Observation,
    /// Published USNO prediction, including the observation-to-forecast transition.
    Forecast,
    /// Linear continuation beyond the published forecast, without an error bound.
    Extrapolation,
}

/// A public scientific source snapshot, independent of the orbital kernels.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct DeltaTDataFile {
    /// Upstream file name.
    pub name: &'static str,
    /// Canonical upstream URL.
    pub source: &'static str,
    /// Identified upstream snapshot.
    pub upstream_version: &'static str,
    /// Retrieval UTC.
    pub retrieved_at: &'static str,
    /// SHA-256 of the verified original bytes.
    pub sha256: &'static str,
    /// Verified original byte count.
    pub bytes: u64,
}

/// A model reading, excluding any original civil date or personal coordinates.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct DeltaTEstimate {
    /// TT minus UT1 in seconds.
    pub seconds: f64,
    /// Observation, published forecast, or explicit model extrapolation.
    pub kind: DeltaTKind,
    /// Quoted source error in seconds; not a guaranteed interpolation error bound.
    /// None means no quoted error, never zero. Between samples, both must report
    /// errors and the larger endpoint error is retained.
    pub reported_error_seconds: Option<f64>,
}

struct Sample {
    jd: f64,
    seconds: f64,
    uncertainty: Option<f64>,
    kind: DeltaTKind,
}
#[allow(clippy::unreadable_literal)] // Generated source literals preserve their original precision.
mod data {
    use super::{DeltaTDataFile, DeltaTKind, Sample};
    include!(concat!(env!("OUT_DIR"), "/delta_t_samples.rs"));
}

/// Verified primary data identities used by this numerical model.
#[must_use]
pub fn delta_t_sources() -> &'static [DeltaTDataFile] {
    data::SOURCES
}

/// Evaluate Delta-T at a UTC Julian day, returning no estimate for nonfinite input
/// or an unrepresentable result. UTC dates are proleptic Gregorian. The historic
/// decimal years use the elapsed fraction of their actual Gregorian year.
#[must_use]
pub fn delta_t_estimate(jd_utc: f64) -> Option<DeltaTEstimate> {
    if !jd_utc.is_finite() {
        return None;
    }
    let samples = data::SAMPLES;
    let first = samples.first()?;
    let last = samples.last()?;
    let estimate = if jd_utc < first.jd {
        DeltaTEstimate {
            seconds: espenak_meeus(decimal_year(jd_utc)),
            kind: DeltaTKind::HistoricalModel,
            reported_error_seconds: None,
        }
    } else if jd_utc > last.jd {
        let before = samples.get(samples.len().checked_sub(2)?)?;
        DeltaTEstimate {
            seconds: interpolate(before, last, jd_utc),
            kind: DeltaTKind::Extrapolation,
            reported_error_seconds: None,
        }
    } else {
        match samples.binary_search_by(|sample| sample.jd.total_cmp(&jd_utc)) {
            Ok(index) => {
                let sample = samples.get(index)?;
                DeltaTEstimate {
                    seconds: sample.seconds,
                    kind: sample.kind,
                    reported_error_seconds: sample.uncertainty,
                }
            }
            Err(index) => {
                let before = samples.get(index.checked_sub(1)?)?;
                let after = samples.get(index)?;
                DeltaTEstimate {
                    seconds: interpolate(before, after, jd_utc),
                    kind: after.kind,
                    reported_error_seconds: before
                        .uncertainty
                        .zip(after.uncertainty)
                        .map(|(a, b)| a.max(b)),
                }
            }
        }
    };
    estimate.seconds.is_finite().then_some(estimate)
}

fn interpolate(before: &Sample, after: &Sample, jd: f64) -> f64 {
    before.seconds + (jd - before.jd) * (after.seconds - before.seconds) / (after.jd - before.jd)
}

/// TT minus UT1 in seconds at a UTC Julian day. Nonfinite inputs/results yield NaN;
/// callers needing explicit availability and source classification use
/// [`delta_t_estimate`]. SPK interpolation itself uses TDB, not this correction.
#[must_use]
pub fn delta_t_seconds(jd_utc: f64) -> f64 {
    delta_t_estimate(jd_utc).map_or(f64::NAN, |estimate| estimate.seconds)
}

/// Actual elapsed fraction of the containing proleptic Gregorian calendar year.
fn decimal_year(jd_utc: f64) -> f64 {
    let year = calendar_year(jd_utc);
    let jan1 = january_first_jd(year) - 0.5;
    let next_jan1 = january_first_jd(year + 1.0) - 0.5;
    year + (jd_utc - jan1) / (next_jan1 - jan1)
}

/// The integer calendar year (as an exact f64) containing the UTC Julian
/// date, inverting the proleptic Gregorian days-from-civil mapping
/// (Hinnant's `civil_from_days`). All arithmetic stays in f64 floor form —
/// the magnitudes are far below 2⁵³, so every value is exact and no casts
/// are needed.
fn calendar_year(jd_utc: f64) -> f64 {
    // Days since 0000-03-01 proleptic Gregorian (JD 1721119.5 = 0000-03-01.0).
    let z = (jd_utc + 0.5).floor() - 1_721_120.0;
    let era = (z / 146_097.0).floor();
    let day_of_era = z - era * 146_097.0;
    let year_of_era = ((day_of_era - (day_of_era / 1_460.0).floor()
        + (day_of_era / 36_524.0).floor()
        - (day_of_era / 146_096.0).floor())
        / 365.0)
        .floor();
    // Day within the (March-anchored) year, then the March-based month, to
    // apply the January/February year correction.
    let day_of_year = day_of_era
        - (365.0 * year_of_era + (year_of_era / 4.0).floor() - (year_of_era / 100.0).floor());
    let month_shift = ((5.0 * day_of_year + 2.0) / 153.0).floor();
    let month = month_shift + if month_shift < 10.0 { 3.0 } else { -9.0 };
    year_of_era + era * 400.0 + if month <= 2.0 { 1.0 } else { 0.0 }
}

/// The Julian date of January 1.5 (noon UTC) of the decimal Gregorian
/// `year` — the standard Fliegel–Van Flandern days-from-civil for
/// (year, January, 1), in f64 floor form (exact below 2⁵³).
fn january_first_jd(year: f64) -> f64 {
    let y = year + 4_799.0;
    1.0 + 306.0 + 365.0 * y + (y / 4.0).floor() - (y / 100.0).floor() + (y / 400.0).floor()
        - 32_045.0
}

/// Espenak & Meeus (2006) piecewise polynomials, ΔT in seconds.
///
/// Published in NASA TP-2009-214174 (Five Millennium Canon of Solar
/// Eclipses), equations for −500…+2050; the pieces below cover the
/// ephemeris kernel's 1550–1955 span. `y` is the decimal Gregorian year.
fn espenak_meeus(y: f64) -> f64 {
    if y < 1600.0 {
        // Canon piece for 500–1600, u = (y − 1000)/100.
        let u = (y - 1000.0) / 100.0;
        1574.2 - 556.01 * u + 71.23472 * u * u + 0.319_781 * u * u * u
            - 0.850_346_3 * u * u * u * u
            - 0.005_050_998 * u * u * u * u * u
            + 0.008_357_207_3 * u * u * u * u * u * u
    } else if y < 1700.0 {
        let t = y - 1600.0;
        120.0 - 0.9808 * t - 0.01532 * t * t + t * t * t / 7129.0
    } else if y < 1800.0 {
        let t = y - 1700.0;
        8.83 + 0.1603 * t - 0.005_928_5 * t * t + 0.000_133_36 * t * t * t
            - t * t * t * t / 1_174_000.0
    } else if y < 1860.0 {
        let t = y - 1800.0;
        13.72 - 0.332_447 * t + 0.006_861_2 * t * t + 0.004_111_6 * t * t * t
            - 0.000_374_36 * t * t * t * t
            + 0.000_012_127_2 * t * t * t * t * t
            - 0.000_000_169_9 * t * t * t * t * t * t
            + 0.000_000_000_875 * t * t * t * t * t * t * t
    } else if y < 1900.0 {
        let t = y - 1860.0;
        7.62 + 0.5737 * t - 0.251_754 * t * t + 0.016_806_68 * t * t * t
            - 0.000_447_362_4 * t * t * t * t
            + t * t * t * t * t / 233_174.0
    } else if y < 1920.0 {
        let t = y - 1900.0;
        -2.79 + 1.494_119 * t - 0.059_893_9 * t * t + 0.006_196_6 * t * t * t
            - 0.000_197 * t * t * t * t
    } else if y < 1941.0 {
        let t = y - 1920.0;
        21.20 + 0.84493 * t - 0.076_100 * t * t + 0.002_093_6 * t * t * t
    } else {
        // Canon piece for 1941–1961, evaluated only up to 1955 here.
        let t = y - 1950.0;
        29.07 + 0.407 * t - t * t / 233.0 + t * t * t / 2547.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_distinguish_quoted_errors_from_unbounded_extrapolation() {
        let observed = delta_t_estimate(2_461_314.5).expect("pinned observation");
        assert_eq!(observed.kind, DeltaTKind::Observation);
        assert!(
            observed.reported_error_seconds.is_none(),
            "monthly data has no quoted error"
        );
        let forecast = delta_t_estimate(2_463_871.5).expect("pinned final prediction");
        assert_eq!(forecast.kind, DeltaTKind::Forecast);
        assert!(
            forecast
                .reported_error_seconds
                .is_some_and(|error| (error - 1.0).abs() < 1e-12),
            "the published prediction's quoted error must be preserved"
        );
        let outside = delta_t_estimate(2_463_872.5).expect("explicit linear continuation");
        assert_eq!(outside.kind, DeltaTKind::Extrapolation);
        assert!(
            outside.reported_error_seconds.is_none(),
            "extrapolation has no stated error bound"
        );
        assert!(
            (outside.seconds - (71.25 + 0.05 / 91.0)).abs() < 1e-9,
            "continuation must use the final published segment, without an invented future fit"
        );
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(
                delta_t_estimate(invalid).is_none(),
                "nonfinite epochs have no estimate"
            );
        }
    }

    #[test]
    fn usno_observation_forecast_and_transition_are_reproduced() {
        // Pinned USNO deltat.data: 2026-10-01 = 69.2065 s.
        // Pinned deltat.preds: MJD 61406 = 69.14 s and MJD 63871 = 71.25 s.
        // The midpoint between the observation and first future forecast is linear.
        for (mjd, expected) in [(61_314.0, 69.2065), (61_360.0, 69.17325), (63_871.0, 71.25)] {
            assert!(
                (delta_t_seconds(mjd + 2_400_000.5) - expected).abs() < 1e-9,
                "the selected primary-source observations and forecasts must be reproduced"
            );
        }
    }

    #[test]
    fn decimal_year_stays_inside_a_leap_year_until_its_last_instant() {
        let last_minute = january_first_jd(2001.0) - 0.5 - 1.0 / 1440.0;
        assert!(
            (2000.0..2001.0).contains(&decimal_year(last_minute)),
            "calendar fractions must use the actual length of a leap year"
        );
    }

    /// 1955 January 1.5 UT, Julian day 2435109.0.
    const JD_1955: f64 = 2_435_109.0;

    /// Julian day at `year` January 1 12:00 UT (Gregorian), matching the
    /// probe epochs used against Swiss Ephemeris. Noon of a calendar day is
    /// exactly the integer Julian day number of that day.
    fn jd_jan1_5(year: i32) -> f64 {
        january_first_jd(f64::from(year))
    }

    /// January 1.0 of the year, for the Gregorian historical-polynomial anchors.
    fn jd_jan1_0(year: i32) -> f64 {
        january_first_jd(f64::from(year)) - 0.5
    }

    /// Swiss Ephemeris 2.10.03 `swe_deltat` (default model), measured with
    /// the `gen_probe` C harness against its own binary files, 2026-10-30.
    /// Post-1955 observational anchors retain the existing comparison limit
    /// against the independently implemented USNO interpolation.
    #[test]
    fn observational_anchors_agree_with_swiss_reference() {
        let cases: [(i32, f64); 7] = [
            (1960, 33.1506),
            (1970, 40.1827),
            (1980, 50.5399),
            (1990, 56.8571),
            (2000, 63.8289),
            (2010, 66.0706),
            (2020, 69.3613),
        ];
        for (year, expected) in cases {
            let got = delta_t_seconds(jd_jan1_5(year));
            assert!(
                (got - expected).abs() < 0.005,
                "year {year}: got {got}, Swiss Ephemeris {expected}"
            );
        }
    }

    /// Before 1955 we use the published Espenak & Meeus polynomials while
    /// Swiss Ephemeris uses the Stephenson/Morrison/Hohenkerk 2016 spline.
    /// The measured gap (probe, 2026-10-30) peaks near 0.85 s around 1900,
    /// which maps to ≈0.0036° of line longitude — inside the 0.01° bar, and
    /// this test pins it so a regression cannot silently exceed the bar.
    #[test]
    fn pre_1955_divergence_from_swiss_ephemeris_stays_inside_bar() {
        // (year, Swiss Ephemeris seconds) from the probe run.
        let cases: [(i32, f64); 6] = [
            (1900, -1.9518),
            (1910, 11.1596),
            (1920, 21.6274),
            (1930, 24.4229),
            (1940, 24.4282),
            (1950, 28.9327),
        ];
        // 0.85 s ≈ 12.8″ ≈ 0.00356° of Earth rotation.
        for (year, se) in cases {
            let got = delta_t_seconds(jd_jan1_5(year));
            assert!(
                (got - se).abs() < 0.9,
                "year {year}: |got {got} − se {se}| exceeds the measured bound"
            );
        }
    }

    /// The published Espenak & Meeus polynomials, evaluated at the year
    /// itself (January 1.0). The reference probe's printed table carried a
    /// stray `+0.004`-year offset in its `dt_EM` column; these expectations
    /// are the polynomials at the year, not that column.
    #[test]
    fn pre_1955_reproduces_published_polynomials() {
        let cases: [(i32, f64); 6] = [
            (1900, -2.79),
            (1910, 10.3884),
            (1920, 21.20),
            (1930, 24.1329),
            (1940, 24.4074),
            (1950, 29.07),
        ];
        for (year, expected) in cases {
            let got = delta_t_seconds(jd_jan1_5(year) - 0.5);
            assert!(
                (got - expected).abs() < 0.001,
                "year {year}: got {got}, published {expected}"
            );
        }
    }

    /// The civil decimal year anchors exactly: January 1.0 of every year is
    /// that year .0, including the non-leap century years where a fixed
    /// 365.2425-day anchor drifts.
    #[test]
    fn decimal_year_anchors_on_january_first() {
        for year in [1550, 1900, 1955, 1987, 2000, 2028, 2100] {
            let got = decimal_year(jd_jan1_0(year));
            assert!(
                (got - f64::from(year)).abs() < 1e-9,
                "year {year}: decimal_year(Jan 1.0) = {got}"
            );
        }
    }

    /// The decimal year advances by the elapsed year fraction and stays
    /// inside the calendar year at both boundaries.
    #[test]
    fn decimal_year_tracks_year_fraction() {
        // June 1.0 of 2000: day 152 of the year (leap year).
        let mid = decimal_year(jd_jan1_0(2000) + 152.0);
        assert!((mid - 2000.0 - 152.0 / 366.0).abs() < 1e-9);
        // December 31 23:59 UTC remains inside 2026.
        let eve = decimal_year(jd_jan1_0(2026) + 364.999);
        assert!((2026.0..2027.0).contains(&eve), "Dec 31 eve: {eve}");
    }

    /// The calendar year helper agrees with the January-first JD formula in
    /// both directions, including leap and non-leap century years.
    #[test]
    fn calendar_year_round_trips() {
        for year in [1550, 1600, 1900, 1901, 2000, 2024, 2100] {
            // A noon instant on March 1 sits unambiguously inside the year.
            let march = january_first_jd(f64::from(year)) + 60.5;
            assert!(
                (calendar_year(march) - f64::from(year)).abs() < 1e-9,
                "March 1 noon of {year}"
            );
        }
    }

    /// The 1955 boundary is continuous: the polynomial era hands over to the
    /// table within a few hundredths of a second (no visible line jump).
    #[test]
    fn boundary_1955_is_continuous() {
        let before = delta_t_seconds(JD_1955 - 1.0);
        let after = delta_t_seconds(JD_1955);
        assert!((after - before).abs() < 0.1, "{before} vs {after}");
    }
}
