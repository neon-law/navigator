//! The mean lunar node (ascending): the intersection line of the Moon's mean
//! orbit with the ecliptic, computed from lunar orbital geometry — the node is
//! not a physical body, so no SPK lookup applies.
//!
//! The port follows Swiss Ephemeris's active mean-node path (verified
//! fixture-exact): Moshier's DE404-fit secular mean elements for the mean
//! longitude (`SWELP`) and the mean distance from the ascending node (`NF`)
//! give the node longitude on the *mean* ecliptic and equinox of date, a
//! piecewise-linear correction table (fit to DE431 over its full range)
//! removes the residual against the numerical solution, and the nutation in
//! longitude is added to refer the result to the *true* equinox of date.
//! Swiss Ephemeris then reports the node on the ecliptic (latitude exactly 0)
//! with RA/Dec obtained by rotating that true-of-date longitude through the
//! true obliquity; this module reproduces that convention. At JD 2415021.0
//! the ported chain reproduces Swiss Ephemeris's internal node longitude to
//! 9 decimal places and the fixture row to well under a milliarcsecond.
//!
//! The nutation values come from the shared IAU 2000B port in
//! [`super::frame`]. Swiss Ephemeris's own nutation model differs from IAU
//! 2000B by up to ~0.2″ over 1900–2050; the checked-in Swiss Ephemeris
//! fixture grid (1″ gate) absorbs that difference with margin.

// Literal fidelity: the Moshier mean-element coefficients and the DE431-fit
// correction table are byte-exact transcriptions of the Swiss Ephemeris
// sources; regrouping or shortening their digits would obscure provenance.
#![allow(clippy::unreadable_literal)]
#![allow(clippy::inconsistent_digit_grouping)]
#![allow(clippy::excessive_precision)]

use super::frame;
use super::ApparentPosition;
use crate::time::TdbSeconds;

/// Seconds per Julian century.
const SECONDS_PER_CENTURY: f64 = 36_525.0 * 86_400.0;
/// Seconds per day.
const SECONDS_PER_DAY: f64 = 86_400.0;
/// Julian date of the J2000 epoch.
const J2000_JD: f64 = 2_451_545.0;
/// 360° in arcseconds: the modulus of swisseph's `mods3600`.
const TURN_AS: f64 = 1_296_000.0;
/// Days per Gregorian century (the correction table's grid step).
const DAYS_PER_GREG_CTY: f64 = 36_524.25;
/// Julian date of the correction-table origin: 1 Jan −13100 (Gregorian).
const CORR_ORIGIN_JD: f64 = -3_063_616.5;
/// DE431 coverage bounds; outside them the correction is zero (swisseph's
/// `JPL_DE431_START`/`JPL_DE431_END`).
const DE431_START_JD: f64 = -3_027_215.5;
const DE431_END_JD: f64 = 7_930_192.5;

/// DE404-fit secular (t², t³, t⁴) corrections in arcseconds for the mean
/// distance from the ascending node — swisseph's active `z[0..=2]` ("F").
const NODE_SECULAR: [f64; 3] = [
    -1.312_045_233_711e+01,
    -1.138_215_912_580e-03,
    -9.646_018_347_184e-06,
];

/// The same for the mean longitude of the Moon — swisseph's active
/// `z[9..=11]` ("L").
const LONGITUDE_SECULAR: [f64; 3] = [
    -5.663_161_722_088e+00,
    5.722_859_298_199e-03,
    -8.466_472_828_815e-05,
];

/// Linear mean elements in arcseconds (Moshier, swisseph's active branch).
/// The huge multiple-of-turn coefficient is evaluated against `fracT` (the
/// fractional part of the centuries since J2000) rather than `T`: it is an
/// exact multiple of 360° per century, so restricting it to the fractional
/// part restores the low-order arcsecond digits that `T × 1.7e9` would lose
/// to floating-point cancellation.
const NODE_FRAC_TERM_AS: f64 = 1_739_232_000.0;
const NODE_LINEAR_AS: f64 = 295_263.098_3 - 2.079_419_901_760e-01;
const NODE_CONSTANT_AS: f64 = 335_779.557_55;
const LONGITUDE_FRAC_TERM_AS: f64 = 1_731_456_000.0;
const LONGITUDE_LINEAR_AS: f64 = 1_108_372.832_64 - 6.784_914_260_953e-01;
const LONGITUDE_CONSTANT_AS: f64 = 785_939.955_71;

/// Piecewise-linear correction to the mean node in degrees, one entry per
/// Gregorian century from `CORR_ORIGIN_JD`, linearly interpolated (swisseph
/// `mean_node_corr`, fit to DE431; the disabled 130-entry block in the C
/// source is excluded). Index `i` accompanies Julian date
/// `CORR_ORIGIN_JD + i × DAYS_PER_GREG_CTY`.
const MEAN_NODE_CORR: [f64; 304] = [
    -2.56, -2.473, -2.392347, -2.316425, -2.239639, -2.167764, -2.095100, -2.024810, -1.957622,
    -1.890097, -1.826389, -1.763335, -1.701047, -1.643016, -1.584186, -1.527309, -1.473352,
    -1.418917, -1.367736, -1.317202, -1.267269, -1.221121, -1.174218, -1.128862, -1.086214,
    -1.042998, -1.002491, -0.962635, -0.923176, -0.887191, -0.850403, -0.814929, -0.782117,
    -0.748462, -0.717241, -0.686598, -0.656013, -0.628726, -0.600460, -0.573219, -0.548634,
    -0.522931, -0.499285, -0.476273, -0.452978, -0.432663, -0.411386, -0.390788, -0.372825,
    -0.353681, -0.336230, -0.319520, -0.302343, -0.287794, -0.272262, -0.257166, -0.244534,
    -0.230635, -0.218126, -0.206365, -0.194000, -0.183876, -0.172782, -0.161877, -0.153254,
    -0.143371, -0.134501, -0.126552, -0.117932, -0.111199, -0.103716, -0.096160, -0.090718,
    -0.084046, -0.078007, -0.072959, -0.067235, -0.062990, -0.058102, -0.053070, -0.049786,
    -0.045381, -0.041317, -0.038165, -0.034501, -0.031871, -0.028844, -0.025701, -0.024018,
    -0.021427, -0.018881, -0.017291, -0.015186, -0.013755, -0.012098, -0.010261, -0.009688,
    -0.008218, -0.006670, -0.005979, -0.004756, -0.003991, -0.002996, -0.001974, -0.001975,
    -0.001213, -0.000377, -0.000356, 5.779e-05, 0.000378, 0.000710, 0.001092, 0.000767, 0.000985,
    0.001443, 0.001069, 0.001141, 0.001321, 0.001462, 0.001695, 0.001319, 0.001567, 0.001873,
    0.001376, 0.001336, 0.001347, 0.001330, 0.001256, 0.000813, 0.000946, 0.001079,
    // The DE431 fit is exact at the middle of its range.
    0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
    0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -0.000364, -0.000452, -0.001091,
    -0.001159, -0.001136, -0.001798, -0.002249, -0.002622, -0.002990, -0.003555, -0.004425,
    -0.004758, -0.005134, -0.006065, -0.006839, -0.007474, -0.008283, -0.009411, -0.010786,
    -0.011810, -0.012989, -0.014825, -0.016426, -0.017922, -0.019774, -0.021881, -0.024194,
    -0.026190, -0.028440, -0.031285, -0.033817, -0.036318, -0.039212, -0.042456, -0.045799,
    -0.048994, -0.052710, -0.056948, -0.061017, -0.065181, -0.069843, -0.074922, -0.079976,
    -0.085052, -0.090755, -0.096840, -0.102797, -0.108939, -0.115568, -0.122636, -0.129593,
    -0.136683, -0.144641, -0.152825, -0.161044, -0.169758, -0.178916, -0.188712, -0.198401,
    -0.208312, -0.219395, -0.230407, -0.241577, -0.253508, -0.265640, -0.278556, -0.291330,
    -0.304353, -0.318815, -0.332882, -0.347316, -0.362895, -0.378421, -0.395061, -0.411748,
    -0.428666, -0.447477, -0.465636, -0.484277, -0.504600, -0.524405, -0.545533, -0.567020,
    -0.588404, -0.612099, -0.634965, -0.658262, -0.683866, -0.708526, -0.734719, -0.761800,
    -0.788562, -0.818092, -0.846885, -0.876177, -0.908385, -0.939371, -0.972027, -1.006149,
    -1.039634, -1.076135, -1.112156, -1.148490, -1.188312, -1.226761, -1.266821, -1.309156,
    -1.350583, -1.395223, -1.440028, -1.485047, -1.534104, -1.582023, -1.631506, -1.684031,
    -1.735687, -1.790421, -1.846039, -1.901951, -1.961872, -2.021179, -2.081987, -2.146259,
    -2.210031, -2.276609, -2.344904, -2.413795, -2.486559, -2.559564, -2.634215, -2.712692,
    -2.791289, -2.872533, -2.956217, -3.040965, -3.129234, -3.218545, -3.309805, -3.404827,
    -3.5008, -3.601, -3.7, -3.8,
];

/// Reduces an arcsecond value into \[0, 360°), swisseph's `mods3600`.
fn mods3600(arcseconds: f64) -> f64 {
    arcseconds - TURN_AS * (arcseconds / TURN_AS).floor()
}

/// The mean-node correction in degrees at Julian date `jd`, linearly
/// interpolated across the Gregorian-century table grid; zero outside DE431
/// coverage (which also keeps the index in bounds: the covered span starts at
/// table index 1 and ends at index 301, so `i + 1` is always valid).
fn mean_node_correction_degrees(jd: f64) -> f64 {
    if !(DE431_START_JD..=DE431_END_JD).contains(&jd) {
        return 0.0;
    }
    let elapsed_centuries = (jd - CORR_ORIGIN_JD) / DAYS_PER_GREG_CTY;
    let index = elapsed_centuries.floor();
    let fraction = elapsed_centuries - index;
    // The range guard above pins `index` to [0, 300]: flooring it cannot
    // truncate, and slot `index + 1` is always in bounds.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let lower = MEAN_NODE_CORR[index as usize];
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let upper = MEAN_NODE_CORR[index as usize + 1];
    lower + fraction * (upper - lower)
}

/// The mean node's ecliptic longitude in degrees, referred to the mean
/// equinox of date, from Moshier's secular mean elements plus the DE431
/// correction table.
#[must_use]
pub fn mean_node_longitude_degrees(t: TdbSeconds) -> f64 {
    let centuries = t.0 / SECONDS_PER_CENTURY;
    let fractional = centuries.fract();
    // Mean distance from the ascending node and mean longitude of the Moon,
    // in arcseconds, linear terms plus the DE404-fit secular corrections.
    let node =
        mods3600(NODE_FRAC_TERM_AS * fractional + (NODE_LINEAR_AS * centuries + NODE_CONSTANT_AS))
            + ((NODE_SECULAR[2] * centuries + NODE_SECULAR[1]) * centuries + NODE_SECULAR[0])
                * centuries
                * centuries;
    let longitude = mods3600(
        LONGITUDE_FRAC_TERM_AS * fractional
            + (LONGITUDE_LINEAR_AS * centuries + LONGITUDE_CONSTANT_AS),
    ) + ((LONGITUDE_SECULAR[2] * centuries + LONGITUDE_SECULAR[1]) * centuries
        + LONGITUDE_SECULAR[0])
        * centuries
        * centuries;
    // The node is the difference of the two longitudes, in degrees.
    let node_degrees = (longitude - node) / 3600.0;
    let jd = J2000_JD + t.0 / SECONDS_PER_DAY;
    (node_degrees - mean_node_correction_degrees(jd)).rem_euclid(360.0)
}

/// The apparent mean lunar node at `t`, in the Swiss Ephemeris `SE_MEAN_NODE`
/// convention: ecliptic longitude of date referred to the *true* equinox
/// (mean longitude plus nutation in longitude), latitude exactly 0 — the node
/// lies on the ecliptic by definition — and RA/Dec obtained by rotating that
/// longitude through the true obliquity of date.
#[must_use]
pub fn mean_node(t: TdbSeconds) -> ApparentPosition {
    let centuries = t.0 / SECONDS_PER_CENTURY;
    let mean_longitude_degrees = mean_node_longitude_degrees(t);
    let (dpsi, _) = frame::nut00b(centuries);
    let longitude_degrees = (mean_longitude_degrees + dpsi.to_degrees()).rem_euclid(360.0);
    let obliquity = frame::true_obliquity(t);
    let (sin_obliquity, cos_obliquity) = obliquity.sin_cos();
    let longitude = longitude_degrees.to_radians();
    let (sin_longitude, cos_longitude) = longitude.sin_cos();
    ApparentPosition {
        ra_deg: (sin_longitude * cos_obliquity)
            .atan2(cos_longitude)
            .to_degrees()
            .rem_euclid(360.0),
        dec_deg: (sin_longitude * sin_obliquity).asin().to_degrees(),
        lon_deg: longitude_degrees,
        lat_deg: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anchor from an instrumented Swiss Ephemeris 2.10.03 build: at
    /// `JD_TT` 2415021.0 (1900-01-01 12h) `swi_mean_lunar_elements` reports the
    /// mean node longitude 259.129986321°, and `swe_calc(SE_MEAN_NODE)`
    /// reports the true-of-date longitude 259.134852588° = that value plus
    /// swisseph's Δψ = 17.518561″. The ported mean elements + correction
    /// table must reproduce the mean longitude to 9 decimal places.
    #[test]
    fn mean_elements_reproduce_the_swisseph_anchor() {
        // JD_TT 2415021.0 expressed as TDB seconds past J2000 (TDB−TT is at
        // most 1.7 ms, far below the comparison precision).
        let t = TdbSeconds((2_415_021.0 - J2000_JD) * SECONDS_PER_DAY);
        let longitude = mean_node_longitude_degrees(t);
        assert!(
            (longitude - 259.129_986_321).abs() < 1e-8,
            "mean node longitude {longitude} vs swisseph 259.129986321"
        );
    }

    /// Meeus's published mean-node polynomial (Astronomical Algorithms ch.
    /// 47): Ω = 125.04452° − 1934.136261°·T + …, quoted to 1e−5 degrees. At
    /// J2000 the ported Moshier elements must agree within that precision
    /// plus the correction-table residual.
    #[test]
    fn mean_node_matches_the_published_table_at_j2000() {
        let longitude = mean_node_longitude_degrees(TdbSeconds(0.0));
        assert!(
            (longitude - 125.044_52).abs() < 1e-4,
            "mean node at J2000 {longitude} vs Meeus 125.04452"
        );
    }

    /// The node lies on the ecliptic by definition; the convention reports
    /// latitude exactly zero, and the longitude must be the true-of-date
    /// value: mean longitude plus the IAU 2000B nutation in longitude.
    // Latitude is set to zero by convention, not computed: exact equality is
    // the contract under test.
    #[allow(clippy::float_cmp)]
    #[test]
    fn node_position_applies_nutation_in_longitude() {
        let t = TdbSeconds((2_415_021.0 - J2000_JD) * SECONDS_PER_DAY);
        let position = mean_node(t);
        let (dpsi, _) = frame::nut00b(t.0 / SECONDS_PER_CENTURY);
        let expected_lon = (mean_node_longitude_degrees(t) + dpsi.to_degrees()).rem_euclid(360.0);
        assert!(
            (position.lon_deg - expected_lon).abs() < 1e-12,
            "node longitude {} vs expected {expected_lon}",
            position.lon_deg
        );
        assert_eq!(position.lat_deg, 0.0, "node latitude is exactly zero");
    }

    /// The correction table's guarded interpolation: zero outside DE431
    /// coverage, in-bounds and continuous inside it. The covered span starts
    /// one table index in (so `index + 1` cannot overflow) and ends two
    /// before the end.
    // The guarded out-of-coverage path returns the literal 0.0: exact
    // equality is the contract under test.
    #[allow(clippy::float_cmp)]
    #[test]
    fn correction_table_stays_in_bounds_across_coverage() {
        assert_eq!(mean_node_correction_degrees(DE431_START_JD - 1.0), 0.0);
        assert_eq!(mean_node_correction_degrees(DE431_END_JD + 1.0), 0.0);
        // Spanning steps of half a century across the whole coverage window.
        let mut jd = DE431_START_JD;
        while jd <= DE431_END_JD {
            let _ = mean_node_correction_degrees(jd);
            jd += DAYS_PER_GREG_CTY / 2.0;
        }
    }

    /// `mods3600` reduces into \[0, 360°) for both signs, matching the C
    /// floor-based modulus.
    #[test]
    fn mods3600_reduces_both_signs() {
        let reduced = mods3600(-3985.66 * 3600.0);
        assert!((0.0..TURN_AS).contains(&reduced), "reduced {reduced}");
        assert!(mods3600(TURN_AS * 42.0 + 1234.5) - 1234.5 < 1e-9);
    }
}
