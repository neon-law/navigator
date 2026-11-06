//! Latitude-clipped, normalized Mercator display geometry, separate from astronomy.
//!
//! The [Google world-coordinate convention](https://developers.google.com/maps/documentation/javascript/coordinates)
//! places the northwest corner at (0, 0) and southeast at (1, 1). Multiplication
//! by 256 gives zoom-zero world pixels. This adapter activates no map provider.
//! Straight projected chords have no inherited horizon-condition certificate.

use ephemeris::{ChartError, GeographicLine};
use std::f64::consts::PI;

/// Resource limit on explicitly requested display worlds, not an astronomy policy.
pub const MAX_WORLD_COPIES: usize = 8;

/// A projected body/angle in one explicit world copy. Private geometry deliberately
/// has neither Debug nor serialization; no epoch, birth input or provenance is copied.
pub struct MercatorLine {
    pub body: &'static str,
    pub source_id: Option<i32>,
    pub angle: &'static str,
    pub line_index: usize,
    pub world_copy: i16,
    pub segments: Vec<MercatorSegment>,
}

/// One continuous visible subpath; indices reference the original canonical line.
pub struct MercatorSegment {
    pub source_segment_index: usize,
    /// Normalized world x/y, eastward/southward, including the integer x offset.
    pub points: Vec<[f64; 2]>,
}

/// The latitude of the square world's horizontal edges: atan(sinh(pi)).
#[must_use]
pub fn latitude_limit_deg() -> f64 {
    PI.sinh().atan().to_degrees()
}

/// Clip canonical coordinate-linear edges to the square Mercator latitude band,
/// then project each retained vertex. Paired antimeridian edges remain x=0 and x=1.
/// Output follows input-line, requested-world, source-segment and subpath order.
/// Empty/tangent-only subpaths and wholly invisible lines are omitted. Outside
/// excursions never reconnect, and the source geometry is never modified.
///
/// # Errors
/// Rejects empty, duplicate or more than eight world-copy requests, and invalid
/// geographic segments. Offsets are bounded integers; no viewport or map settings
/// are inferred. Errors contain no coordinates or private calculation metadata.
pub fn project_lines(
    lines: &[GeographicLine],
    world_copies: &[i16],
) -> Result<Vec<MercatorLine>, ChartError> {
    if world_copies.is_empty()
        || world_copies.len() > MAX_WORLD_COPIES
        || world_copies
            .iter()
            .enumerate()
            .any(|(i, copy)| world_copies[..i].contains(copy))
    {
        return Err(ChartError::InvalidGeometry);
    }
    let mut output = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        let segments = project_segments(line.segments())?;
        if segments.is_empty() {
            continue;
        }
        for &world_copy in world_copies {
            output.push(MercatorLine {
                body: line.body(),
                source_id: line.source_id(),
                angle: line.angle(),
                line_index,
                world_copy,
                segments: segments
                    .iter()
                    .map(|segment| MercatorSegment {
                        source_segment_index: segment.source_segment_index,
                        points: segment
                            .points
                            .iter()
                            .map(|p| [p[0] + f64::from(world_copy), p[1]])
                            .collect(),
                    })
                    .collect(),
            });
        }
    }
    Ok(output)
}

fn project_segments(segments: &[Vec<[f64; 2]>]) -> Result<Vec<MercatorSegment>, ChartError> {
    let mut output = Vec::new();
    for (source_segment_index, segment) in segments.iter().enumerate() {
        if segment.len() < 2 || segment.iter().any(|p| !valid_point(*p)) {
            return Err(ChartError::InvalidGeometry);
        }
        let mut path = Vec::new();
        for edge in segment.windows(2) {
            let (a, b) = (edge[0], edge[1]);
            // Canonical segments never cross the world seam internally.
            if (a[0] - b[0]).abs() > 180.0 {
                return Err(ChartError::InvalidGeometry);
            }
            if let Some((start, end)) = clip_edge(a, b) {
                let start = project(start)?;
                let end = project(end)?;
                if path.last().is_some_and(|last| !same_point(*last, start)) {
                    flush(&mut output, &mut path, source_segment_index);
                }
                if path.is_empty() {
                    path.push(start);
                }
                if !same_point(start, end) {
                    path.push(end);
                }
            } else {
                flush(&mut output, &mut path, source_segment_index);
            }
            if b[1].abs() > latitude_limit_deg() {
                flush(&mut output, &mut path, source_segment_index);
            }
        }
        flush(&mut output, &mut path, source_segment_index);
    }
    Ok(output)
}

fn valid_point(p: [f64; 2]) -> bool {
    p[0].is_finite()
        && (-180.0..=180.0).contains(&p[0])
        && p[1].is_finite()
        && (-90.0..=90.0).contains(&p[1])
}

fn same_point(a: [f64; 2], b: [f64; 2]) -> bool {
    // Signed zeros represent the same point.
    (a[0] - b[0]).abs() <= 0.0 && (a[1] - b[1]).abs() <= 0.0
}

fn flush(output: &mut Vec<MercatorSegment>, path: &mut Vec<[f64; 2]>, source_segment_index: usize) {
    if path.len() >= 2 {
        output.push(MercatorSegment {
            source_segment_index,
            points: std::mem::take(path),
        });
    } else {
        path.clear();
    }
}

fn clip_edge(mut a: [f64; 2], mut b: [f64; 2]) -> Option<([f64; 2], [f64; 2])> {
    let limit = latitude_limit_deg();
    if (a[1] > limit && b[1] > limit) || (a[1] < -limit && b[1] < -limit) {
        return None;
    }
    // A tangent intersection is the supplied cap endpoint itself. Recomputing it
    // at t=0/1 can create a microscopic projected edge through cancellation.
    if a[1].abs() > limit && b[1].to_bits() == limit.copysign(a[1]).to_bits() {
        return Some((b, b));
    }
    if b[1].abs() > limit && a[1].to_bits() == limit.copysign(b[1]).to_bits() {
        return Some((a, a));
    }
    // Intersections follow the canonical straight WGS84 lon/lat edge, never a
    // spherical arc or the chord between independently clamped endpoints.
    let original_a = a;
    let original_b = b;
    if a[1].abs() > limit {
        let latitude = limit.copysign(a[1]);
        let t = (latitude - original_a[1]) / (original_b[1] - original_a[1]);
        a = [
            original_a[0] + t * (original_b[0] - original_a[0]),
            latitude,
        ];
    }
    if b[1].abs() > limit {
        let latitude = limit.copysign(b[1]);
        let t = (latitude - original_a[1]) / (original_b[1] - original_a[1]);
        b = [
            original_a[0] + t * (original_b[0] - original_a[0]),
            latitude,
        ];
    }
    Some((a, b))
}

fn project(p: [f64; 2]) -> Result<[f64; 2], ChartError> {
    const BOUNDARY_ROUNDOFF: f64 = 16.0 * f64::EPSILON;
    let x = (p[0] + 180.0) / 360.0;
    let y = 0.5 - p[1].to_radians().tan().asinh() / (2.0 * PI);
    // Geometric clipping already bounded latitude. Only analytic boundary roundoff
    // (a few f64 ulps) is removed here; this is not scalar geographic clamping.
    if !y.is_finite() || !(-BOUNDARY_ROUNDOFF..=1.0 + BOUNDARY_ROUNDOFF).contains(&y) {
        return Err(ChartError::InvalidGeometry);
    }
    Ok([x, y.clamp(0.0, 1.0)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opposite_polar_endpoints_keep_the_band_crossing() {
        let output = project_segments(&[vec![[-60.0, -90.0], [120.0, 90.0]]]).unwrap();
        assert_eq!(
            output.len(),
            1,
            "the visible crossing must survive clipping"
        );
        let points = &output[0].points;
        assert_eq!(points.len(), 2);
        // Linear geographic interpolation: longitude = latitude + 30 degrees.
        let limit = PI.sinh().atan().to_degrees();
        assert!((points[0][0] - (-limit + 210.0) / 360.0).abs() < 1e-14);
        assert!((points[1][0] - (limit + 210.0) / 360.0).abs() < 1e-14);
        assert!((points[0][1] - 1.0).abs() < 1e-14);
        assert!(points[1][1].abs() < 1e-14);
    }

    fn inverse(point: [f64; 2]) -> [f64; 2] {
        [
            point[0] * 360.0 - 180.0,
            ((0.5 - point[1]) * 2.0 * PI).sinh().atan().to_degrees(),
        ]
    }

    #[test]
    fn outside_excursions_remain_separate_and_intersections_follow_geographic_edges() {
        let input = vec![[-120.0, 80.0], [-80.0, 90.0], [-40.0, 80.0], [0.0, 0.0]];
        let before = input.clone();
        let output = project_segments(std::slice::from_ref(&input)).unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].source_segment_index, 0);
        assert_eq!(output[1].source_segment_index, 0);
        let exit = inverse(*output[0].points.last().unwrap());
        let enter = inverse(output[1].points[0]);
        let delta = (latitude_limit_deg() - 80.0) * 4.0;
        assert!((exit[0] - (-120.0 + delta)).abs() < 1e-12);
        assert!((enter[0] - (-40.0 - delta)).abs() < 1e-12);
        assert!((exit[1] - latitude_limit_deg()).abs() < 1e-12);
        assert_eq!(input, before, "source coordinates remain untouched");
    }

    #[test]
    fn projection_anchors_inverse_and_paired_seam_edges_are_distinct() {
        for (geographic, expected) in [
            ([-180.0, 0.0], [0.0, 0.5]),
            ([0.0, 0.0], [0.5, 0.5]),
            ([180.0, 0.0], [1.0, 0.5]),
            // tan(45 + phi/2) = exp(pi/2) gives y=1/4 independently.
            (
                [
                    90.0,
                    (2.0 * (PI / 2.0).exp().atan() - PI / 2.0).to_degrees(),
                ],
                [0.75, 0.25],
            ),
        ] {
            let actual = project(geographic).unwrap();
            for axis in 0..2 {
                assert!((actual[axis] - expected[axis]).abs() < 1e-14);
                assert!((inverse(actual)[axis] - geographic[axis]).abs() < 1e-12);
            }
        }
        let output = project_segments(&[
            vec![[179.123_456, 10.0], [180.0, 20.0]],
            vec![[-180.0, 20.0], [-179.123_456, 30.0]],
        ])
        .unwrap();
        assert_eq!(output.len(), 2);
        assert_eq!(output[0].source_segment_index, 0);
        assert_eq!(output[1].source_segment_index, 1);
        assert!((output[0].points[1][0] - 1.0).abs() < 1e-15);
        assert!(output[1].points[0][0].abs() < 1e-15);
    }

    #[test]
    fn invisible_and_zero_length_tangencies_do_not_become_paths() {
        let limit = latitude_limit_deg();
        for segment in [
            vec![[0.0, 86.0], [20.0, 90.0]],
            vec![[0.0, -86.0], [20.0, -90.0]],
            vec![[0.0, 90.0], [10.0, limit], [20.0, 90.0]],
            vec![[0.0, 0.0], [0.0, 0.0]],
        ] {
            assert!(project_segments(&[segment]).unwrap().is_empty());
        }
        let boundary = project_segments(&[vec![[0.0, limit], [10.0, limit]]]).unwrap();
        assert_eq!(boundary.len(), 1, "nonzero boundary paths stay visible");
        assert_eq!(boundary[0].points.len(), 2);
    }

    #[test]
    fn invalid_geography_and_unbounded_or_duplicate_world_requests_fail_closed() {
        for point in [
            [f64::NAN, 0.0],
            [0.0, f64::INFINITY],
            [181.0, 0.0],
            [0.0, 91.0],
        ] {
            assert!(project_segments(&[vec![[0.0, 0.0], point]]).is_err());
        }
        assert!(project_segments(&[vec![[0.0, 0.0]]]).is_err());
        assert!(project_segments(&[vec![[179.0, 0.0], [-179.0, 0.0]]]).is_err());
        for worlds in [&[][..], &[0, 0][..], &[0, 1, 2, 3, 4, 5, 6, 7, 8][..]] {
            assert!(project_lines(&[], worlds).is_err());
        }
        assert!(project_lines(&[], &[i16::MIN, 0, i16::MAX])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn fractional_tangent_endpoints_do_not_gain_roundoff_paths() {
        let limit = latitude_limit_deg();
        for edge in [
            vec![[-179.9, 90.0], [-51.2, limit]],
            vec![[-51.2, limit], [-179.9, 90.0]],
            vec![[-179.9, -90.0], [-51.2, -limit]],
            vec![[-51.2, -limit], [-179.9, -90.0]],
        ] {
            assert!(
                project_segments(&[edge]).unwrap().is_empty(),
                "a single tangent endpoint is not a path"
            );
        }
    }

    #[test]
    fn exact_opposite_cap_endpoint_keeps_the_full_band_crossing() {
        let limit = latitude_limit_deg();
        for edge in [
            vec![[-100.0, 90.0], [20.0, -limit]],
            vec![[20.0, -limit], [-100.0, 90.0]],
            vec![[-100.0, -90.0], [20.0, limit]],
            vec![[20.0, limit], [-100.0, -90.0]],
        ] {
            let output = project_segments(&[edge]).unwrap();
            assert_eq!(output.len(), 1, "the opposite-cap crossing must survive");
            assert_eq!(output[0].points.len(), 2);
            assert!((output[0].points[1][1] - output[0].points[0][1]).abs() > 0.99);
        }
    }
}
