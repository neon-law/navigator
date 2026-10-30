//! MC and IC vertical lines: the meridians of culmination.
//!
//! A body culminates (crosses the local meridian, the MC angle) where its
//! local hour angle is zero, and anti-culminates (the IC angle) half a turn
//! of longitude away. Both loci are geodetic meridians — curves of constant
//! longitude — so the polylines are the full pole-to-pole runs of a single
//! longitude each.

use super::{Angle, AngularLine, LineBody, Point};

/// Latitude step of the vertical polylines, in degrees. The meridian is
/// straight in longitude, but dense vertices keep it correct under any map
/// projection that bends meridians on screen.
const LATITUDE_STEP_DEG: f64 = 1.0;

/// The longitude where `body`'s hour angle vanishes (the MC meridian):
/// λ = RA − GAST, east-positive, wrapped into \[0, 360).
///
/// Local sidereal time equals the body's right ascension exactly at
/// culmination, and LST = GAST + λ for east-positive λ.
#[must_use]
pub fn mc_longitude_degrees(ra_deg: f64, gast_deg: f64) -> f64 {
    (ra_deg - gast_deg).rem_euclid(360.0)
}

/// The longitude of the IC meridian: the MC longitude shifted by 180°,
/// wrapped into \[0, 360).
#[must_use]
pub fn ic_longitude_degrees(ra_deg: f64, gast_deg: f64) -> f64 {
    (mc_longitude_degrees(ra_deg, gast_deg) + 180.0).rem_euclid(360.0)
}

/// Both culmination longitudes in one call.
#[must_use]
pub fn mc_ic_longitudes(ra_deg: f64, gast_deg: f64) -> (f64, f64) {
    let mc = mc_longitude_degrees(ra_deg, gast_deg);
    let ic = (mc + 180.0).rem_euclid(360.0);
    (mc, ic)
}

/// A pole-to-pole vertical line at `longitude_deg`.
///
/// Longitudes stay single-valued here; the antimeridian split happens at the
/// [`super::split_antimeridian`] boundary of the public API, where it also
/// applies to curved lines.
#[must_use]
pub fn vertical_line(body: LineBody, angle: Angle, longitude_deg: f64) -> AngularLine {
    let mut points = Vec::with_capacity(181);
    let mut latitude = -90.0;
    while latitude < 90.0 {
        points.push(Point(longitude_deg, latitude));
        latitude += LATITUDE_STEP_DEG;
    }
    points.push(Point(longitude_deg, 90.0));
    AngularLine {
        body,
        angle,
        points,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Wrap arithmetic: RA ahead of GAST gives an east-of-Greenwich MC.
    #[test]
    fn mc_longitude_wraps_into_half_open_range() {
        // The probe instant 2000-01-01 12:00 UT: Sun RA 281.278385579,
        // GAST 280.457072443 (gen_probe, 2026-10-30).
        let mc = mc_longitude_degrees(281.278_385_579, 280.457_072_443);
        assert!((mc - 0.821_313_136).abs() < 1e-6);
        assert!((0.0..360.0).contains(&mc));

        // Negative intermediate values still wrap correctly.
        let wrapped = mc_longitude_degrees(10.0, 20.0);
        assert!((wrapped - 350.0).abs() < 1e-12);
    }

    /// IC is exactly half a turn away, with wrap-around.
    #[test]
    fn ic_opposes_mc() {
        let (mc, ic) = mc_ic_longitudes(281.278_385_579, 280.457_072_443);
        assert!((ic - mc - 180.0).abs() < 1e-9);
        assert!((ic - 180.821_313_136).abs() < 1e-6);

        let (mc2, ic2) = mc_ic_longitudes(5.0, 0.0);
        assert!((mc2 - 5.0).abs() < 1e-12);
        assert!((ic2 - 185.0).abs() < 1e-12);
    }

    /// Vertical lines run pole to pole at constant longitude.
    #[test]
    fn vertical_line_spans_poles() {
        let line = vertical_line(LineBody::Sun, Angle::Mc, 123.5);
        assert_eq!(line.points.len(), 181);
        assert_eq!(line.points[0], Point(123.5, -90.0));
        assert_eq!(line.points[90], Point(123.5, 0.0));
        assert_eq!(line.points[180], Point(123.5, 90.0));
        for point in &line.points {
            assert!((point.0 - 123.5).abs() < 1e-12);
        }
    }
}
