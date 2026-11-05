//! Mean ascending lunar node from IERS Conventions (2003), Simon et al. (1994).
//!
//! The mean longitude follows ERFA 2.0.1 `faom03`, evaluated in TDB Julian
//! centuries since J2000. IAU 2000B nutation refers it to the true equinox
//! of date; IAU 2006 true obliquity supplies right ascension and declination.
//! The node lies on the ecliptic, so its latitude is zero.
//!
//! Source: <https://github.com/liberfa/erfa/blob/v2.0.1/src/faom03.c>.
//! ERFA is derived from SOFA; see the repository `NOTICE` for its notice.

use super::{frame, ApparentPosition};
use crate::time::TdbSeconds;

const SECONDS_PER_CENTURY: f64 = 36_525.0 * 86_400.0;

/// Mean ecliptic longitude, in degrees in [0, 360), using IERS 2003.
#[must_use]
pub fn mean_node_longitude_degrees(t: TdbSeconds) -> f64 {
    let centuries = t.0 / SECONDS_PER_CENTURY;
    let arcseconds = 450_160.398_036
        + centuries
            * (-6_962_890.543_1
                + centuries * (7.4722 + centuries * (0.007_702 - centuries * 0.000_059_39)));
    (arcseconds / 3600.0).rem_euclid(360.0)
}

/// Mean lunar node referred to the true equinox of date.
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

    /// ERFA 2.0.1 `t_erfa_c.c::t_faom03`: IERS 2003 node at TDB T=0.8.
    #[test]
    fn mean_node_reproduces_the_iers_reference() {
        let actual = mean_node_longitude_degrees(TdbSeconds(0.8 * SECONDS_PER_CENTURY));
        let expected = (-5.973_618_440_951_302_f64).to_degrees().rem_euclid(360.0);
        assert!(
            (actual - expected).abs() < 1e-10,
            "mean node {actual} vs IERS reference {expected}"
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
        let t = TdbSeconds(-36_524.0 * 86_400.0);
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
}
