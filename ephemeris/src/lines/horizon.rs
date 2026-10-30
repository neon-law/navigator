//! ASC and DSC horizon curves: where the body stands exactly on the horizon.
//!
//! The set of surface points where a body's geocentric altitude is zero is a
//! great circle: the horizon circle, whose pole is the sub-body point
//! (latitude δ, longitude `λ_sub` = RA − GAST — the same longitude as the MC
//! meridian). Each longitude λ carries exactly one horizon-circle point,
//! found from the hour angle H = λ − `λ_sub` and the horizon condition
//! sin φ sin δ + cos φ cos δ cos H = 0, which solves to
//! tan φ = −cos H / tan δ.
//!
//! The culmination meridian (`λ_sub` and its antipode) cuts the circle into
//! two semicircles — the astrocartography angles:
//!
//! * **ASC** — the eastern semicircle, λ from `λ_sub` to `λ_sub` + 180°: it
//!   leaves the MC meridian on the side opposite the body's hemisphere, at
//!   90° − |δ| from the equator, crosses the equator 90° east of the
//!   sub-body point, and meets the IC meridian on the body's own side.
//! * **DSC** — the western semicircle, the mirror image through the west.
//!
//! For an exactly equatorial body (δ = 0) the circle degenerates to the two
//! meridians 90° east and west of the sub-body point — the body then rises
//! along an entire meridian at once — and the curves are emitted as
//! [`super::vertical`] pole-to-pole lines instead.

use super::{Angle, AngularLine, LineBody, Point};

/// Base longitude sampling step of the horizon polylines, in degrees.
const BASE_STEP_DEG: f64 = 0.5;

/// Finest longitude step of the horizon polylines, in degrees. Refinement
/// splits only intervals wider than twice this value, so every emitted gap
/// stays at or above it.
const FINE_STEP_DEG: f64 = 0.1;

/// Chord-deviation threshold, in degrees of latitude, above which an
/// interval is split: the sampled polyline must track the true curve more
/// closely than the 0.01° golden-file bar, with margin for the renderer.
const CHORD_DEVIATION_DEG: f64 = 0.002;

/// Refinement pass cap. Each pass halves intervals, and resolving the base
/// 0.5° step down to the 0.1° floor needs at most ⌈log₂(0.5/0.1)⌉ = 3
/// effective halvings per interval; 12 leaves a wide margin while bounding
/// the loop for any pathological curvature.
const MAX_REFINEMENT_PASSES: usize = 12;

/// Declination, in degrees, below which the horizon locus is treated as
/// exactly equatorial and emitted as two meridians. Below this the f64
/// tangent underflows toward 0/0 at the quarter-turn longitudes, and the
/// smooth family is cartographically indistinguishable from the degenerate
/// meridians anyway.
const EQUATORIAL_EPSILON_DEG: f64 = 1e-12;

/// The horizon-locus latitude at longitude `lambda_deg` for a body whose
/// sub-body point sits at `sub_lon_deg` longitude and `dec_deg` declination.
///
/// tan φ = −cos H / tan δ with the east-positive hour angle
/// H = λ − `λ_sub`. Returns a latitude strictly inside (−90°, 90°) for any
/// nonzero declination.
#[must_use]
fn horizon_latitude_degrees(lambda_deg: f64, dec_deg: f64, sub_lon_deg: f64) -> f64 {
    let hour_rad = (lambda_deg - sub_lon_deg).to_radians();
    let phi_rad = (-hour_rad.cos() / dec_deg.to_radians().tan()).atan();
    phi_rad.to_degrees()
}

/// One semicircle of the horizon circle, sampled adaptively in longitude.
///
/// The arc runs east-positive from `lambda_start` to `lambda_end` (a 180°
/// span). Coarse 0.5° sampling is refined wherever the curve departs from
/// its chord by more than [`CHORD_DEVIATION_DEG`], down to the 0.1° floor —
/// the steep near-vertical transitions of near-equatorial bodies.
#[must_use]
fn sample_horizon_arc(
    lambda_start: f64,
    lambda_end: f64,
    dec_deg: f64,
    sub_lon_deg: f64,
) -> Vec<Point> {
    let mut lambdas = Vec::new();
    // Index-multiplication grid: no cumulative drift over the 360 steps of a
    // semicircle, so unrefined gaps stay exactly at the base step.
    let mut step = 0u32;
    loop {
        let lambda = lambda_start + f64::from(step) * BASE_STEP_DEG;
        if lambda >= lambda_end - BASE_STEP_DEG / 2.0 {
            break;
        }
        lambdas.push(lambda);
        step += 1;
    }
    lambdas.push(lambda_end);

    for _ in 0..MAX_REFINEMENT_PASSES {
        let mut changed = false;
        let mut refined = Vec::with_capacity(lambdas.len() * 2);
        for window in lambdas.windows(2) {
            let (a, b) = (window[0], window[1]);
            refined.push(a);
            if b - a > 2.0 * FINE_STEP_DEG {
                let midpoint = f64::midpoint(a, b);
                let chord_mid = f64::midpoint(
                    horizon_latitude_degrees(a, dec_deg, sub_lon_deg),
                    horizon_latitude_degrees(b, dec_deg, sub_lon_deg),
                );
                let true_mid = horizon_latitude_degrees(midpoint, dec_deg, sub_lon_deg);
                if (true_mid - chord_mid).abs() > CHORD_DEVIATION_DEG {
                    refined.push(midpoint);
                    changed = true;
                }
            }
        }
        refined.push(lambdas.last().copied().unwrap_or(lambda_end));
        if !changed {
            break;
        }
        lambdas = refined;
    }

    lambdas
        .iter()
        .map(|&lon| Point(lon, horizon_latitude_degrees(lon, dec_deg, sub_lon_deg)))
        .collect()
}

/// The body's ASC and DSC horizon curves.
///
/// `dec_deg` is the body's apparent declination of date and `mc_lon_deg` the
/// MC meridian longitude from [`super::vertical::mc_longitude_degrees`] —
/// together they locate the sub-body point whose horizon circle carries both
/// curves. For an exactly equatorial body the pair degenerates to two
/// pole-to-pole meridians 90° east and west of `mc_lon_deg`.
#[must_use]
pub fn asc_dsc_curves(body: LineBody, dec_deg: f64, mc_lon_deg: f64) -> (AngularLine, AngularLine) {
    if dec_deg.abs() < EQUATORIAL_EPSILON_DEG {
        let asc = super::vertical::vertical_line(body, Angle::Asc, mc_lon_deg + 90.0);
        let dsc = super::vertical::vertical_line(body, Angle::Dsc, mc_lon_deg - 90.0);
        return (asc, dsc);
    }

    // Eastern semicircle: MC junction → east equator point → IC junction.
    let asc_points = sample_horizon_arc(mc_lon_deg, mc_lon_deg + 180.0, dec_deg, mc_lon_deg);
    // Western semicircle: the mirror run through the west equator point.
    let dsc_points = sample_horizon_arc(mc_lon_deg - 180.0, mc_lon_deg, dec_deg, mc_lon_deg);

    (
        AngularLine {
            body,
            angle: Angle::Asc,
            points: asc_points,
        },
        AngularLine {
            body,
            angle: Angle::Dsc,
            points: dsc_points,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::{split_antimeridian, wrap_longitude};

    /// The horizon circle's junction latitude with the MC meridian: on the
    /// side opposite the body's hemisphere, 90° − |δ| from the equator.
    /// (Solved from tan φ = −1/tan δ at H = 0.)
    #[must_use]
    fn mc_junction_latitude_degrees(dec_deg: f64) -> f64 {
        -dec_deg.signum() * (90.0 - dec_deg.abs())
    }

    /// The horizon circle's junction latitude with the IC meridian: on the
    /// body's own side of the equator, 90° − |δ| from it. (tan φ = +1/tan δ
    /// at H = 180°.)
    #[must_use]
    fn ic_junction_latitude_degrees(dec_deg: f64) -> f64 {
        dec_deg.signum() * (90.0 - dec_deg.abs())
    }

    /// Sub-body point for the probe instant: Sun at declination −23.07°,
    /// MC longitude 0.821° (RA 281.278, GAST 280.457 — `gen_probe`, 2026-10-30).
    const SUN_DEC: f64 = -23.07;
    const SUN_MC_LON: f64 = 0.821_313_136;

    /// Every sampled vertex lies on the horizon great circle: its angular
    /// distance to the sub-body point is exactly 90°. This is the load-bearing
    /// geometric invariant of both curves.
    #[test]
    fn vertices_sit_on_the_horizon_great_circle() {
        for (dec, sub_lon) in [
            (SUN_DEC, SUN_MC_LON),
            (23.44, 100.0),
            (-5.0, 359.0),
            (0.5, 42.0),
        ] {
            let (asc, dsc) = asc_dsc_curves(LineBody::Sun, dec, sub_lon);
            for line in [&asc, &dsc] {
                assert!(!line.points.is_empty());
                for point in &line.points {
                    let dot = great_circle_dot(point.0, point.1, sub_lon, dec);
                    // dot = cos(angular distance) = 0 at exactly 90°.
                    assert!(
                        dot.abs() < 1e-9,
                        "lon {} lat {} dec {dec}: dot {dot}",
                        point.0,
                        point.1
                    );
                }
            }
        }
    }

    /// Dot product of the unit vectors of a surface point and a sub-body
    /// point: zero exactly when the body stands on the horizon there.
    fn great_circle_dot(lon: f64, lat: f64, sub_lon: f64, sub_lat: f64) -> f64 {
        let (lon_r, lat_r) = (lon.to_radians(), lat.to_radians());
        let (sub_lon_r, sub_lat_r) = (sub_lon.to_radians(), sub_lat.to_radians());
        lat_r.cos() * sub_lat_r.cos() * (lon_r - sub_lon_r).cos() + lat_r.sin() * sub_lat_r.sin()
    }

    /// The curves join the culmination meridians exactly: at each junction
    /// the horizon circle crosses the meridian on the anti-body side
    /// (MC) and the body's own side (IC), 90° − |δ| from the equator.
    #[test]
    fn junctions_meet_the_culmination_meridians() {
        let (asc, dsc) = asc_dsc_curves(LineBody::Sun, SUN_DEC, SUN_MC_LON);

        let first = asc.points[0];
        assert!((wrap_longitude(first.0) - SUN_MC_LON).abs() < 1e-9);
        assert!((first.1 - mc_junction_latitude_degrees(SUN_DEC)).abs() < 1e-9);

        let last = asc.points[asc.points.len() - 1];
        assert!((wrap_longitude(last.0) - wrap_longitude(SUN_MC_LON + 180.0)).abs() < 1e-9);
        assert!((last.1 - ic_junction_latitude_degrees(SUN_DEC)).abs() < 1e-9);

        // DSC shares both junctions with ASC, approached from the west.
        assert!(
            (wrap_longitude(dsc.points[0].0) - wrap_longitude(SUN_MC_LON + 180.0)).abs() < 1e-9
        );
        assert!((dsc.points[0].1 - ic_junction_latitude_degrees(SUN_DEC)).abs() < 1e-9);
        let dsc_last = dsc.points[dsc.points.len() - 1];
        assert!((wrap_longitude(dsc_last.0) - SUN_MC_LON).abs() < 1e-9);
        assert!((dsc_last.1 - mc_junction_latitude_degrees(SUN_DEC)).abs() < 1e-9);

        // Southern declination mirrors the geometry: the southern-body MC
        // junction sits in the north and the IC junction in the south.
        let (asc_s, _) = asc_dsc_curves(LineBody::Sun, -SUN_DEC, SUN_MC_LON);
        assert!((asc_s.points[0].1 - mc_junction_latitude_degrees(-SUN_DEC)).abs() < 1e-9);
        let last_s = asc_s.points[asc_s.points.len() - 1];
        assert!((last_s.1 - ic_junction_latitude_degrees(-SUN_DEC)).abs() < 1e-9);
    }

    /// Each curve crosses the equator a quarter turn of longitude from the
    /// sub-body point: ASC due east, DSC due west.
    #[test]
    fn equator_crossing_a_quarter_turn_away() {
        let (asc, dsc) = asc_dsc_curves(LineBody::Sun, SUN_DEC, SUN_MC_LON);

        let asc_equator = asc
            .points
            .iter()
            .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .expect("asc has points");
        assert!(
            (wrap_longitude(asc_equator.0) - wrap_longitude(SUN_MC_LON + 90.0)).abs() < 0.5,
            "ASC equator crossing longitude {}",
            asc_equator.0
        );
        assert!(asc_equator.1.abs() < 0.1);

        let dsc_equator = dsc
            .points
            .iter()
            .min_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .expect("dsc has points");
        assert!(
            (wrap_longitude(dsc_equator.0) - wrap_longitude(SUN_MC_LON - 90.0)).abs() < 0.5,
            "DSC equator crossing longitude {}",
            dsc_equator.0
        );
        assert!(dsc_equator.1.abs() < 0.1);
    }

    /// An exactly equatorial body rises along whole meridians: the curves
    /// become the two pole-to-pole verticals 90° east and west of the
    /// sub-body meridian.
    #[test]
    fn equatorial_body_degenerates_to_meridians() {
        let (asc, dsc) = asc_dsc_curves(LineBody::Moon, 0.0, SUN_MC_LON);
        assert_eq!(asc.points.len(), 181);
        assert_eq!(asc.points[0], Point(SUN_MC_LON + 90.0, -90.0));
        assert_eq!(asc.points[180], Point(SUN_MC_LON + 90.0, 90.0));
        assert_eq!(dsc.points[0], Point(SUN_MC_LON - 90.0, -90.0));
    }

    /// Adaptive sampling: flat stretches stay at the 0.5° base step while
    /// steep stretches near the equator crossing are refined toward 0.1°.
    #[test]
    fn sampling_refines_steep_stretches() {
        // A near-equatorial body: the transition through the equator is steep.
        let (asc, _) = asc_dsc_curves(LineBody::Moon, 1.0, SUN_MC_LON);
        let mut gaps: Vec<f64> = asc
            .points
            .windows(2)
            .map(|pair| pair[1].0 - pair[0].0)
            .collect();
        gaps.sort_by(f64::total_cmp);

        let finest = gaps[0];
        let coarsest = gaps[gaps.len() - 1];
        assert!(finest < 0.15, "steep stretch refined: finest step {finest}");
        assert!(
            (coarsest - BASE_STEP_DEG).abs() < 1e-9,
            "flat stretch stays coarse: coarsest step {coarsest}"
        );
        // The floor is respected everywhere.
        assert!(finest >= FINE_STEP_DEG - 1e-9);
    }

    /// DSC is the mirror of ASC about the sub-body meridian: equal latitudes
    /// at equal longitude offsets on opposite sides of it.
    #[test]
    fn dsc_mirrors_asc_about_the_sub_body_meridian() {
        let (asc, dsc) = asc_dsc_curves(LineBody::Sun, SUN_DEC, SUN_MC_LON);
        for offset in [10.0, 30.0, 60.0, 89.0] {
            let asc_lat = latitude_at_offset(&asc, offset);
            let dsc_lat = latitude_at_offset(&dsc, -offset);
            assert!(
                (asc_lat - dsc_lat).abs() < 0.05,
                "offset {offset}: asc {asc_lat} dsc {dsc_lat}"
            );
        }
    }

    /// The curve latitude `offset` degrees east of the sub-body meridian
    /// (negative for west), by linear interpolation into the sampled polyline.
    fn latitude_at_offset(line: &AngularLine, offset: f64) -> f64 {
        let target = SUN_MC_LON + offset;
        for pair in line.points.windows(2) {
            if pair[0].0 <= target && target <= pair[1].0 {
                let span = pair[1].0 - pair[0].0;
                let weight = (target - pair[0].0) / span;
                return pair[0].1 + weight * (pair[1].1 - pair[0].1);
            }
        }
        f64::NAN
    }

    /// An arc crossing the antimeridian splits into drawable segments, and
    /// every emitted vertex still sits on the horizon circle.
    #[test]
    fn antimeridian_crossing_splits_segments() {
        // Sub-body point at 170°E: the ASC arc runs 170°E → 350°E, crossing
        // the ±180° seam.
        let (asc, _) = asc_dsc_curves(LineBody::Sun, 20.0, 170.0);
        let segments = asc.segments();
        assert_eq!(segments.len(), 2, "one split at the seam");
        for segment in &segments {
            for point in segment {
                let dot = great_circle_dot(point.0, point.1, 170.0, 20.0);
                assert!(dot.abs() < 1e-9);
            }
        }
        // The split points are distinct vertices; nothing was dropped.
        let total: usize = segments.iter().map(Vec::len).sum();
        assert_eq!(total, asc.points.len());
        assert_eq!(split_antimeridian(&asc.points).len(), 2);
    }
}
