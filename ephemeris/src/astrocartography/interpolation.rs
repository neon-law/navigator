//! Continuous bounds for coordinate-linear horizon edges, not projected paths.

use crate::lines::{wrap_longitude, Angle};
use crate::ChartError;

pub(super) const BUDGET_DEG: f64 = 0.001;
/// Allowance for supported-platform f64 trigonometry and coordinate conversion.
/// This is separate from the real-arithmetic remainder proof and source accuracy.
pub(super) const SINE_ROUNDOFF: f64 = 1e-12;
const MAX_POINTS: usize = 16_384;
const MAX_DEPTH: usize = 48;
type Point = [f64; 2];

struct Refinement {
    declination: f64,
    meridian: f64,
    rising: bool,
    points_left: usize,
    max_depth: usize,
}

pub(super) fn refine(
    segments: Vec<Vec<Point>>,
    declination: f64,
    meridian: f64,
    angle: Angle,
) -> Result<Vec<Vec<Point>>, ChartError> {
    let mut refinement = Refinement {
        declination,
        meridian,
        rising: angle == Angle::Asc,
        points_left: MAX_POINTS,
        max_depth: MAX_DEPTH,
    };
    segments
        .into_iter()
        .map(|segment| refinement.segment(&segment))
        .collect()
}

impl Refinement {
    fn segment(&mut self, original: &[Point]) -> Result<Vec<Point>, ChartError> {
        let mut result = Vec::with_capacity(original.len());
        if let Some(first) = original.first() {
            self.push(&mut result, *first)?;
        }
        for edge in original.windows(2) {
            self.edge(&mut result, edge[0], edge[1], 0)?;
        }
        Ok(result)
    }

    fn push(&mut self, result: &mut Vec<Point>, point: Point) -> Result<(), ChartError> {
        if self.points_left == 0
            || !point.iter().all(|coordinate| coordinate.is_finite())
            || !(-180.0..=180.0).contains(&point[0])
            || !(-90.0..=90.0).contains(&point[1])
            || sine_altitude(point, self.declination, self.meridian).abs() + SINE_ROUNDOFF
                > super::VERTEX_TOLERANCE_DEG.to_radians()
        {
            return Err(ChartError::InvalidGeometry);
        }
        self.points_left -= 1;
        result.push(point);
        Ok(())
    }

    fn edge(
        &mut self,
        result: &mut Vec<Point>,
        a: Point,
        b: Point,
        depth: usize,
    ) -> Result<(), ChartError> {
        if certified(a, b, self.declination, self.meridian) {
            return self.push(result, b);
        }
        if depth >= self.max_depth {
            return Err(ChartError::InvalidGeometry);
        }
        let middle = self.midpoint(a, b)?;
        self.edge(result, a, middle, depth + 1)?;
        self.edge(result, middle, b, depth + 1)
    }

    fn midpoint(&self, a: Point, b: Point) -> Result<Point, ChartError> {
        if same_point(a, b) {
            return Err(ChartError::InvalidGeometry);
        }
        let longitude = f64::midpoint(a[0], b[0]);
        let latitude = f64::midpoint(a[1], b[1]);
        let point = if self.declination.abs() < 1e-12 {
            // The existing equatorial convention supplies constant meridians.
            [longitude, latitude]
        } else if (b[1] - a[1]).abs() > (b[0] - a[0]).abs() {
            // Latitude remains well-conditioned where longitude barely changes.
            let cosine_hour = -latitude.to_radians().tan() * self.declination.to_radians().tan();
            if !cosine_hour.is_finite() || cosine_hour.abs() > 1.0 + 1e-12 {
                return Err(ChartError::InvalidGeometry);
            }
            let hour = cosine_hour.clamp(-1.0, 1.0).acos().to_degrees();
            let mut lon = wrap_longitude(self.meridian + if self.rising { -hour } else { hour });
            // Choose the existing split segment's world copy, including its seam edge.
            lon += ((longitude - lon) / 360.0).round() * 360.0;
            [lon, latitude]
        } else {
            let hour = (longitude - self.meridian).to_radians();
            [
                longitude,
                (-hour.cos() / self.declination.to_radians().tan())
                    .atan()
                    .to_degrees(),
            ]
        };
        if same_point(point, a)
            || same_point(point, b)
            || !point.iter().all(|value| value.is_finite())
        {
            return Err(ChartError::InvalidGeometry);
        }
        Ok(point)
    }
}

// Bitwise identity detects subdivision that cannot produce a new stored vertex.
fn same_point(a: Point, b: Point) -> bool {
    a.map(f64::to_bits) == b.map(f64::to_bits)
}

fn sine_altitude(point: Point, declination: f64, meridian: f64) -> f64 {
    let (phi, delta, hour) = (
        point[1].to_radians(),
        declination.to_radians(),
        wrap_longitude(point[0] - meridian).to_radians(),
    );
    phi.sin() * delta.sin() + phi.cos() * delta.cos() * hour.cos()
}

fn radians_upper(degrees: f64) -> f64 {
    (degrees.abs().next_up() * (std::f64::consts::PI.next_up() / 180.0).next_up()).next_up()
}

/// For coordinate-linear phi/H on t∈[0,1], S=sin(phi)sin(delta)+cos(phi)cos(delta)cos(H).
/// Differentiating twice gives |S''|≤dphi²+(|dphi|+|dlambda|)²=M, using unit
/// coefficient bounds. The linear-interpolation remainder is ≤M*t*(1-t)/2≤M/8.
/// Thus endpoint residual plus M/8 bounds EVERY interior sine-altitude, not samples.
/// Nonnegative arithmetic is inflated upward; the target uses sin(x)≥x-x³/6.
fn certified(a: Point, b: Point, declination: f64, meridian: f64) -> bool {
    let bound = sine_bound(a, b, declination, meridian);
    let target = ((BUDGET_DEG.next_down() * std::f64::consts::PI.next_down()).next_down() / 180.0)
        .next_down();
    let cube_upper = ((target * target).next_up() * target).next_up();
    let sine_lower = (target - (cube_upper / 6.0).next_up()).next_down();
    bound.is_finite() && bound <= sine_lower
}

fn sine_bound(a: Point, b: Point, declination: f64, meridian: f64) -> f64 {
    let phi = radians_upper(b[1] - a[1]);
    let lambda = radians_upper(b[0] - a[0]);
    let sum = (phi + lambda).next_up();
    let second_derivative = ((phi * phi).next_up() + (sum * sum).next_up()).next_up();
    let endpoints = sine_altitude(a, declination, meridian)
        .abs()
        .max(sine_altitude(b, declination, meridian).abs());
    ((endpoints + SINE_ROUNDOFF).next_up() + (second_derivative / 8.0).next_up()).next_up()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::{horizon, LineBody};

    // Independently evaluate sky altitude through a unit-vector dot product.
    fn independent_sine(point: Point, declination: f64, meridian: f64) -> f64 {
        let longitude = point[0].to_radians();
        let latitude = point[1].to_radians();
        let declination = declination.to_radians();
        let meridian = meridian.to_radians();
        let location = [
            latitude.cos() * longitude.cos(),
            latitude.cos() * longitude.sin(),
            latitude.sin(),
        ];
        let direction = [
            declination.cos() * meridian.cos(),
            declination.cos() * meridian.sin(),
            declination.sin(),
        ];
        location
            .into_iter()
            .zip(direction)
            .map(|(a, b)| a * b)
            .sum()
    }

    #[test]
    fn analytic_remainder_dominates_independent_interior_evaluations() {
        for declination in [-89.999, -20.0, -0.01, 0.0, 0.01, 20.0, 89.999] {
            for (a, b) in [
                ([-179.0, -89.0], [-178.0, 89.0]),
                ([0.0, 0.0], [170.0, 80.0]),
                ([17.1, 21.0], [17.100_001, 21.000_001]),
            ] {
                let bound = sine_bound(a, b, declination, 17.123_456_789);
                for index in 0..=100 {
                    let fraction = f64::from(index) / 100.0;
                    let point = [
                        a[0] + fraction * (b[0] - a[0]),
                        a[1] + fraction * (b[1] - a[1]),
                    ];
                    assert!(independent_sine(point, declination, 17.123_456_789).abs() <= bound);
                }
            }
        }
    }

    #[test]
    fn extreme_horizon_arcs_preserve_vertices_and_certify_every_edge() {
        for declination in [
            -89.999_999,
            -20.0,
            -0.01,
            -1e-10,
            -1e-12,
            0.0,
            1e-12,
            1e-10,
            0.01,
            20.0,
            89.999_999,
        ] {
            for meridian in [-179.999_999, 17.123_456_789, 179.999_999] {
                let (asc, dsc) = horizon::asc_dsc_curves(LineBody::Sun, declination, meridian);
                for line in [asc, dsc] {
                    let originals: Vec<Vec<Point>> = line
                        .segments()
                        .into_iter()
                        .map(|segment| segment.into_iter().map(|p| [p.0, p.1]).collect())
                        .collect();
                    let refined = refine(originals.clone(), declination, meridian, line.angle)
                        .expect("finite supported arcs refine");
                    assert_eq!(refined.len(), originals.len());
                    for (segment, original) in refined.iter().zip(&originals) {
                        let mut retained = segment.iter();
                        for point in original {
                            assert!(
                                retained.any(|candidate| same_point(*candidate, *point)),
                                "original order and vertices survive"
                            );
                        }
                        for edge in segment.windows(2) {
                            assert!(certified(edge[0], edge[1], declination, meridian));
                            for fraction in [0.125, 0.5, 0.875] {
                                let point = [
                                    edge[0][0] + fraction * (edge[1][0] - edge[0][0]),
                                    edge[0][1] + fraction * (edge[1][1] - edge[0][1]),
                                ];
                                assert!(
                                    independent_sine(point, declination, meridian)
                                        .asin()
                                        .to_degrees()
                                        .abs()
                                        <= BUDGET_DEG
                                );
                            }
                        }
                    }
                    assert!(refined.iter().map(Vec::len).sum::<usize>() <= MAX_POINTS);
                }
            }
        }
    }

    #[test]
    fn resource_and_precision_limits_fail_closed_without_partial_geometry() {
        let mut refinement = Refinement {
            declination: 0.01,
            meridian: 0.0,
            rising: true,
            points_left: 1,
            max_depth: MAX_DEPTH,
        };
        let (line, _) = horizon::asc_dsc_curves(LineBody::Sun, 0.01, 0.0);
        let edge = line
            .points
            .windows(2)
            .find(|edge| !certified([edge[0].0, edge[0].1], [edge[1].0, edge[1].1], 0.01, 0.0))
            .unwrap();
        let points = [[edge[0].0, edge[0].1], [edge[1].0, edge[1].1]];
        assert!(matches!(
            refinement.segment(&points),
            Err(ChartError::InvalidGeometry)
        ));
        refinement.points_left = MAX_POINTS;
        refinement.max_depth = 0;
        assert!(matches!(
            refinement.segment(&points),
            Err(ChartError::InvalidGeometry)
        ));
        assert!(matches!(
            refinement.midpoint([0.0, 90.0], [0.0, 90.0]),
            Err(ChartError::InvalidGeometry)
        ));
        assert!(matches!(
            refinement.segment(&[[f64::NAN, 0.0]]),
            Err(ChartError::InvalidGeometry)
        ));
    }
}
