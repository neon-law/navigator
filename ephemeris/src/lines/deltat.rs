//! Classical ΔT = TT − UT1, the Earth-rotation time correction.
//!
//! UT1 is the Earth-rotation angle's time scale; TT (and its barycentric
//! sibling TDB) paces the ephemeris. Their difference ΔT must come from a
//! model because Earth's spin rate is not predictable far ahead. The model
//! here follows the Swiss Ephemeris 2.10.03 default (what astro.com serves)
//! so that rendered lines agree with the reference charts, while every
//! coefficient comes from a published, unlicensed-concern-free source:
//!
//! * **Before 1955** — the Espenak & Meeus (2006) piecewise polynomials,
//!   published in NASA TP-2009-214174 (*Five Millennium Canon of Solar
//!   Eclipses*), public-domain US-government work. Swiss Ephemeris uses the
//!   Stephenson/Morrison/Hohenkerk 2016 spline in this era; the two models
//!   disagree by at most ≈0.85 s in 1900–1950 (measured against
//!   `swe_deltat`, this is ≈12.8″ ≈ 0.0036° of line longitude — inside the
//!   0.01° validation bar, and recorded honestly in the test fixtures).
//! * **1955 through 2028** — the Astronomical Almanac K8–9 / IERS yearly
//!   table with Bessel's fourth-order interpolation, exactly the numbers and
//!   algorithm Swiss Ephemeris uses (`deltat_aa` in `swephlib.c`), so this
//!   era reproduces the reference to well under a millisecond.
//! * **Beyond 2028** — the published future cubic fitted by
//!   Stephenson/Morrison/Hohenkerk 2016 (UKHO lunar-velocity movement data,
//!   <http://astro.ukho.gov.uk/nao/lvm/>), spliced to the table's last value
//!   over a 100-year transition, as Swiss Ephemeris does.
//!
//! Values before 1600 carry minutes of observational uncertainty; the /world
//! page warns for birth dates where ΔT uncertainty dominates line placement.

/// The ΔT values for 1955 through 2028, in seconds, one per year.
///
/// Transcribed from the tabulated `dt[]` initializer in Swiss Ephemeris
/// 2.10.03 `swephlib.c`, which credits The Astronomical Almanac pp. K8–K9
/// (1997 and later editions) with recent values from IERS
/// (`maia.usno.navy.mil/ser7/deltat.data`). The 2024–2028 entries are that
/// source's extrapolation. The values are published yearly facts, identical
/// in every ephemeris that cites the Almanac.
const ALMANAC_1955_2028: [f64; 74] = [
    31.07, 31.35, 31.68, 32.18, 32.68, 33.15, 33.59, 34.00, 34.47, 35.03, 35.73, 36.54, 37.43,
    38.29, 39.20, 40.18, 41.17, 42.23, 43.37, 44.4841, 45.4761, 46.4567, 47.5214, 48.5344,
    49.5862, 50.5387, 51.3808, 52.1668, 52.9565, 53.7882, 54.3427, 54.8713, 55.3222, 55.8197,
    56.3000, 56.8553, 57.5653, 58.3092, 59.1218, 59.9845, 60.7854, 61.6287, 62.2951, 62.9659,
    63.4673, 63.8285, 64.0908, 64.2998, 64.4734, 64.5736, 64.6876, 64.8452, 65.1464, 65.4574,
    65.7768, 66.0699, 66.3246, 66.6030, 66.9069, 67.2810, 67.6439, 68.1024, 68.5927, 68.9676,
    69.2202, 69.3612, 69.3593, 69.2945, 69.1833, 69.10, 69.00, 68.90, 68.80, 68.80,
];

/// First year of the Almanac table.
const TABLE_START: i32 = 1955;

/// Last year of the Almanac table.
const TABLE_END: i32 = 2028;

/// ΔT at the civil instant given as a UTC Julian date.
///
/// The Julian date must lie on the UTC scale (as produced by
/// `Epoch::to_jde_utc_days`). The returned value is seconds of TT − UT1; the
/// caller converts to days by dividing by 86 400.
///
/// The model is evaluated at the UTC instant itself rather than iterating on
/// UT1: dΔT/dt is at most ≈1 s/year, so the first-order UT1 correction
/// changes ΔT by well under a millisecond — three orders below the line
/// validation bar.
#[must_use]
pub fn delta_t_seconds(jd_utc: f64) -> f64 {
    // Swiss Ephemeris anchors the Almanac-table year fraction on 2451544.5
    // (2000 January 1.0, `deltat_aa`) with a Julian year; keep it for exact
    // parity.
    let y_table = 2000.0 + (jd_utc - 2_451_544.5) / 365.25;
    // The polynomial pieces key on the Gregorian decimal year: the calendar
    // year plus the elapsed fraction of that year, so January 1.0 of year N
    // is exactly N.0.
    let y_gregorian = decimal_year(jd_utc);

    if y_table < f64::from(TABLE_START) {
        espenak_meeus(y_gregorian)
    } else if y_table <= f64::from(TABLE_END) {
        bessel_almanac(y_table)
    } else {
        smh2016_future(y_table)
    }
}

/// The Gregorian decimal year of a UTC Julian date: the calendar year plus
/// the elapsed fraction of that year, so January 1.0 of year N is exactly
/// N.0 — the convention the Espenak–Meeus pieces use for `t = y − 1900`.
fn decimal_year(jd_utc: f64) -> f64 {
    let year = calendar_year(jd_utc);
    let jan1_0 = january_first_jd(year) - 0.5;
    year + (jd_utc - jan1_0) / 365.2425
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
    let day_of_year = day_of_era - (365.0 * year_of_era + (year_of_era / 4.0).floor() - (year_of_era / 100.0).floor());
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
        -2.79 + 1.494_119 * t - 0.059_893_9 * t * t + 0.006_196_6 * t * t * t - 0.000_197 * t * t * t * t
    } else if y < 1941.0 {
        let t = y - 1920.0;
        21.20 + 0.84493 * t - 0.076_100 * t * t + 0.002_093_6 * t * t * t
    } else {
        // Canon piece for 1941–1961, evaluated only up to 1955 here.
        let t = y - 1950.0;
        29.07 + 0.407 * t - t * t / 233.0 + t * t * t / 2547.0
    }
}

/// Astronomical Almanac K8–9 table values with Bessel interpolation.
///
/// A faithful port of the published fourth-order Bessel scheme (Almanac
/// K8–9; identical in Swiss Ephemeris `deltat_aa`): the value at the start
/// of the year, refined with first, Bessel-averaged second, third, and
/// fourth differences of the yearly table. Boundary years where the
/// difference stencil would leave the table fall back to lower order,
/// exactly as the reference does.
fn bessel_almanac(year: f64) -> f64 {
    let table = &ALMANAC_1955_2028;
    let table_len = table.len();
    let start_year = year.floor();
    // The dispatch guarantees 1955 ≤ year ≤ 2028, so the offset is a small
    // non-negative integer; the assert makes the cast's precondition real.
    let offset = start_year - f64::from(TABLE_START);
    assert!(
        (0.0..=f64::from(TABLE_END - TABLE_START)).contains(&offset),
        "dispatch constrains the Almanac year to the table"
    );
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let index = offset as usize;
    let fraction = year - start_year;

    // Zeroth-order estimate is the value at the start of the year.
    let mut estimate = table[index];

    // First order: linear interpolation into the year.
    if index + 1 >= table_len {
        return estimate;
    }
    estimate += fraction * (table[index + 1] - table[index]);

    // Second and higher differences need one year on each side.
    if index == 0 || index + 2 >= table_len {
        return estimate;
    }

    // Five first differences spanning two years on each side of the window.
    let mut first_diffs = [0.0_f64; 6];
    for (offset, slot) in first_diffs.iter_mut().enumerate().take(5) {
        // d[k] = table[k+1] − table[k], with k running index−2 … index+2.
        let diff_index = (index + offset).checked_sub(2);
        if let Some(k) = diff_index.filter(|k| k + 1 < table_len) {
            *slot = table[k + 1] - table[k];
        }
    }

    // Second differences.
    for slot in 0..4 {
        first_diffs[slot] = first_diffs[slot + 1] - first_diffs[slot];
    }
    let mut bessel_weight = 0.25 * fraction * (fraction - 1.0);
    estimate += bessel_weight * (first_diffs[1] + first_diffs[2]);

    // Third differences.
    for slot in 0..3 {
        first_diffs[slot] = first_diffs[slot + 1] - first_diffs[slot];
    }
    bessel_weight *= 2.0 / 3.0;
    estimate += (fraction - 0.5) * bessel_weight * first_diffs[1];

    // Fourth differences (the reference re-checks the left boundary here).
    if index < 2 || index + 3 > table_len {
        return estimate;
    }
    for slot in 0..2 {
        first_diffs[slot] = first_diffs[slot + 1] - first_diffs[slot];
    }
    bessel_weight *= 0.125 * (fraction + 1.0) * (fraction - 2.0);
    estimate += bessel_weight * (first_diffs[0] + first_diffs[1]);

    estimate
}

/// Future extrapolation beyond the Almanac table (after 2028).
///
/// The cubic `64 + 521B/3000 + B²/1250 + 121B³/30000000` (B = y − 2000) is
/// the published fit to the Stephenson/Morrison/Hohenkerk 2016 long-term
/// data (UKHO), and the parabola `42.5 + 32.5·(0.01(y−2000))²` continues it
/// after 2500. To avoid a discontinuity at the table's end, the value is
/// blended linearly into the table's last entry over the century from 2128
/// back to 2028 — the same splice Swiss Ephemeris applies.
fn smh2016_future(y: f64) -> f64 {
    let future_cubic = |b: f64| {
        b * b * b * 121.0 / 30_000_000.0 + b * b / 1250.0 + b * 521.0 / 3000.0 + 64.0
    };
    let mut ans;
    let at_table_end;
    if y < 2500.0 {
        ans = future_cubic(y - 2000.0);
        at_table_end = future_cubic(f64::from(TABLE_END) - 2000.0);
    } else {
        let b = 0.01 * (y - 2000.0);
        ans = b * b * 32.5 + 42.5;
        // The parabola replaces the cubic only far in the future; the
        // blend below then never applies (y ≥ 2500 > table end + 100).
        at_table_end = f64::NAN;
    }
    if y <= f64::from(TABLE_END + 100) {
        let last = ALMANAC_1955_2028[ALMANAC_1955_2028.len() - 1];
        ans += (at_table_end - last) * (y - f64::from(TABLE_END + 100)) * 0.01;
    }
    ans
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1955 January 1.5 UT, Julian day 2435109.0.
    const JD_1955: f64 = 2_435_109.0;

    /// Julian day at `year` January 1 12:00 UT (Gregorian), matching the
    /// probe epochs used against Swiss Ephemeris. Noon of a calendar day is
    /// exactly the integer Julian day number of that day.
    fn jd_jan1_5(year: i32) -> f64 {
        january_first_jd(f64::from(year))
    }

    /// January 1.0 of the year: the instant the Almanac table and the
    /// Espenak–Meeus pieces anchor their yearly values to.
    fn jd_jan1_0(year: i32) -> f64 {
        january_first_jd(f64::from(year)) - 0.5
    }

    /// Swiss Ephemeris 2.10.03 `swe_deltat` (default model), measured with
    /// the `gen_probe` C harness against its own binary files, 2026-10-30.
    /// Post-1955 values must match at the sub-millisecond level because the
    /// model is identical (same table, same interpolation).
    #[test]
    fn post_1955_matches_swiss_ephemeris_default() {
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

    /// The future branch reproduces Swiss Ephemeris beyond the table
    /// (2030 and 2040 anchors from the same probe run).
    #[test]
    fn future_matches_swiss_ephemeris_default() {
        let cases: [(i32, f64); 2] = [(2030, 69.2767), (2040, 71.8002)];
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
        assert!((mid - 2000.0 - 152.0 / 365.2425).abs() < 1e-9);
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

    /// Table interpolation reproduces the yearly values at January 1.5,
    /// where the reference anchors its year fraction.
    #[test]
    fn table_reproduces_yearly_values() {
        let got = delta_t_seconds(jd_jan1_5(1970));
        assert!((got - 40.18).abs() < 0.01, "got {got}");
    }
}
