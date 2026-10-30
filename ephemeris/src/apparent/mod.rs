//! Apparent geocentric positions: where a body appears in the sky from the
//! Earth's center, computed from the barycentric SPK positions.
//!
//! The chain applies, in order:
//!
//! 1. **Light time** ([`light_time`]) — the body is evaluated at the emission
//!    instant whose light reaches the geocenter at the requested time.
//! 2. **Annual aberration** ([`aberration`]) — the geocenter's barycentric
//!    velocity tilts the received direction by up to 20.5″.
//! 3. **Bias-precession-nutation** ([`frame`]) — IAU 2006 precession with
//!    frame bias and IAU 2000B nutation rotate the direction onto the true
//!    equator and equinox of date.
//!
//! [`apparent_geocentric`] returns the resulting right ascension and
//! declination of date, plus the ecliptic longitude and latitude of date
//! (rotated by the true obliquity) used for zodiac positions.
//!
//! The mean lunar node is a point of lunar orbital geometry rather than a
//! physical body, so it bypasses that chain: [`node`] derives its longitude
//! from Moshier's mean elements plus a DE431-fit correction and reports it in
//! the Swiss Ephemeris `SE_MEAN_NODE` convention (see [`mean_node`]).

pub(crate) mod aberration;
pub(crate) mod frame;
pub(crate) mod light_time;
pub(crate) mod node;

use crate::error::EphemerisError;
use crate::spk::{Body, Ephemeris};
use crate::time::TdbSeconds;

/// The aberrated unit direction from the geocenter at `t` toward `body`,
/// evaluated at its light-time-corrected emission instant, in ICRF/J2000
/// components.
///
/// # Errors
///
/// Returns SPK lookup errors when a body is unknown or the requested instant
/// (including the light-time lookback) falls outside kernel coverage, and the
/// light-time convergence error from [`light_time`].
pub(crate) fn direction_icrf(
    ephemeris: &Ephemeris,
    body: Body,
    t: TdbSeconds,
) -> Result<[f64; 3], EphemerisError> {
    let earth_bary_km = ephemeris.barycentric_position(Body::Earth, t)?;
    let solution = light_time::light_time_corrected(ephemeris, body, t, &earth_bary_km)?;
    let earth_velocity_km_s = aberration::earth_velocity_km_s(ephemeris, t)?;
    let unit = unit_vector(solution.geometric_km);
    Ok(aberration::aberrate(unit, &earth_velocity_km_s))
}

/// The apparent geocentric position of `body` at `t`: where it appears in the
/// sky from the Earth's center.
///
/// Right ascension and declination refer to the true equator and equinox of
/// date (IAU 2006/2000B); ecliptic longitude and latitude refer to the
/// ecliptic of date, rotated from the equatorial components by the true
/// obliquity. All angles are in degrees, with right ascension and longitude
/// wrapped into \[0, 360).
///
/// # Errors
///
/// Returns the same errors as [`direction_icrf`] — SPK lookup failures and
/// light-time non-convergence.
pub fn apparent_geocentric(
    ephemeris: &Ephemeris,
    body: Body,
    t: TdbSeconds,
) -> Result<ApparentPosition, EphemerisError> {
    let icrf = direction_icrf(ephemeris, body, t)?;
    let rotation = frame::bias_precession_nutation(t);
    let obliquity = frame::true_obliquity(t);
    Ok(ApparentPosition::from_direction(
        &icrf, &rotation, obliquity,
    ))
}

/// The apparent mean lunar node at `t`, in the Swiss Ephemeris
/// `SE_MEAN_NODE` convention: the Moon's mean ascending node, referred to the
/// true equinox of date, with latitude exactly zero and RA/Dec from the
/// true-of-date ecliptic longitude. See [`node`] for the full convention and
/// the Swiss Ephemeris equivalence proof.
///
/// Unlike [`apparent_geocentric`], no ephemeris kernels are involved: the
/// node follows from lunar orbital geometry alone.
#[must_use]
pub fn mean_node(t: TdbSeconds) -> ApparentPosition {
    node::mean_node(t)
}

/// Apparent geocentric position angles, all in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApparentPosition {
    /// Right ascension on the true equator of date, \[0, 360).
    pub ra_deg: f64,
    /// Declination on the true equator of date, \[−90, 90].
    pub dec_deg: f64,
    /// Ecliptic longitude of date, \[0, 360).
    pub lon_deg: f64,
    /// Ecliptic latitude of date, \[−90, 90].
    pub lat_deg: f64,
}

impl ApparentPosition {
    /// Decomposes a unit direction vector, expressed in the true equator and
    /// equinox of date, into the equatorial and ecliptic angles of date.
    /// `rotation` maps ICRF/J2000 components onto components of date, and
    /// `obliquity` is the true obliquity used for the ecliptic rotation.
    pub(crate) fn from_direction(
        icrf: &[f64; 3],
        rotation: &[[f64; 3]; 3],
        obliquity: f64,
    ) -> ApparentPosition {
        let unit = unit_vector(*icrf);
        let of_date = [
            rotation[0][0] * unit[0] + rotation[0][1] * unit[1] + rotation[0][2] * unit[2],
            rotation[1][0] * unit[0] + rotation[1][1] * unit[1] + rotation[1][2] * unit[2],
            rotation[2][0] * unit[0] + rotation[2][1] * unit[1] + rotation[2][2] * unit[2],
        ];
        // Rotating the coordinate frame about the +x axis by the obliquity
        // carries equatorial components onto the ecliptic of date; the
        // solstice point at right ascension 90° picks up latitude −ε.
        let (sin_obliquity, cos_obliquity) = obliquity.sin_cos();
        let ecliptic_y = of_date[1] * cos_obliquity + of_date[2] * sin_obliquity;
        let ecliptic_z = -of_date[1] * sin_obliquity + of_date[2] * cos_obliquity;
        ApparentPosition {
            ra_deg: wrap_degrees(of_date[1].atan2(of_date[0]).to_degrees()),
            dec_deg: clamp_asin(of_date[2]).to_degrees(),
            lon_deg: wrap_degrees(ecliptic_y.atan2(of_date[0]).to_degrees()),
            lat_deg: clamp_asin(ecliptic_z).to_degrees(),
        }
    }
}

/// Wraps an angle in degrees into \[0, 360).
fn wrap_degrees(angle: f64) -> f64 {
    angle.rem_euclid(360.0)
}

/// The arcsine of a component, clamped into \[−1, 1]. A unit vector's
/// components can exceed 1 by a few ulps after rotation arithmetic; clamping
/// keeps the result finite instead of panicking or returning NaN.
fn clamp_asin(component: f64) -> f64 {
    component.clamp(-1.0, 1.0).asin()
}

/// Scales `v` to unit length. Division by zero is impossible for kernel
/// positions: every ephemeris body's geocentric distance is strictly positive.
fn unit_vector(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / length, v[1] / length, v[2] / length]
}

#[cfg(test)]
// Anchor digits are byte-exact liberfa %.17e printouts; regrouping or
// shortening them would obscure provenance.
#[allow(clippy::unreadable_literal)]
#[allow(clippy::inconsistent_digit_grouping)]
#[allow(clippy::excessive_precision)]
mod tests {
    use super::*;
    use crate::spk::test_support::synthetic_type2;

    const DAY: f64 = 86_400.0;

    /// An ephemeris of two synthetic kernels: Earth fixed at the SSB and the
    /// Moon receding along +x at 10 km/s. With a stationary observer the
    /// aberration is zero, so the apparent direction points exactly at the
    /// emission-time position.
    #[test]
    fn stationary_observer_sees_the_emission_position() {
        let half = DAY / 2.0;
        let v = 10.0_f64;
        let c1 = v * DAY / 2.0;
        let c0 = 1_000_000.0 + c1;
        let x = [c0, c1];
        // All three axes in a type-2 record carry the same coefficient count.
        let zero = [0.0];
        let zero2 = [0.0, 0.0];
        let earth = synthetic_type2(399, 0, 0.0, DAY, &[(half, half, &zero, &zero, &zero)]);
        let moon = synthetic_type2(301, 0, 0.0, DAY, &[(half, half, &x, &zero2, &zero2)]);
        let ephemeris = Ephemeris::new(vec![earth, moon]);

        let t = DAY / 2.0;
        let direction = direction_icrf(&ephemeris, Body::Moon, TdbSeconds(t))
            .expect("synthetic bodies resolve");
        // The direction is exactly +x: the body is on the x axis, the observer
        // is fixed, and the aberration therefore contributes nothing.
        assert!(
            direction[0] > 0.999_999_999
                && direction[1].abs() < 1e-15
                && direction[2].abs() < 1e-15,
            "expected the unit +x direction, got {direction:?}"
        );
    }

    #[test]
    fn unit_vector_normalizes() {
        let unit = unit_vector([3.0, 4.0, 0.0]);
        let length = (unit[0] * unit[0] + unit[1] * unit[1] + unit[2] * unit[2]).sqrt();
        assert!(
            (length - 1.0).abs() < 1e-15,
            "unit vector length {length} must be 1"
        );
    }

    /// The J2000 anchor column: the ICRF +x direction carried onto the true
    /// equator and equinox of date by the ERFA bias-precession-nutation
    /// matrix (first column, from liberfa's `eraFw2m`), and the ERFA true
    /// obliquity at J2000 (mean obliquity plus the nutation in obliquity;
    /// the nutation term is negative here).
    const J2000_COLUMN: [f64; 3] = [
        9.999_999_977_212_139_51e-01,
        -6.189_911_088_842_041_94e-05,
        -2.694_572_645_905_283_26e-05,
    ];
    /// The true obliquity at J2000, from the ERFA anchor digits: mean
    /// obliquity plus the nutation in obliquity, which is negative here
    /// (ERFA's `deps`).
    const J2000_TRUE_OBLIQUITY: f64 = 4.090_926_006_005_828_90e-01 - 2.797_092_331_098_565_26e-05;

    /// The four angles of date expected for the ICRF +x direction at J2000,
    /// computed from the transcribed anchor column rather than from the crate
    /// under test.
    fn j2000_plus_x_angles() -> [f64; 4] {
        let [x, y, z] = J2000_COLUMN;
        let (sin_o, cos_o) = J2000_TRUE_OBLIQUITY.sin_cos();
        let ecliptic_y = y * cos_o + z * sin_o;
        let ecliptic_z = -y * sin_o + z * cos_o;
        [
            y.atan2(x).to_degrees().rem_euclid(360.0),
            z.asin().to_degrees(),
            ecliptic_y.atan2(x).to_degrees().rem_euclid(360.0),
            ecliptic_z.asin().to_degrees(),
        ]
    }

    #[test]
    fn from_direction_matches_the_j2000_anchor() {
        let rotation = [
            [
                9.999_999_977_212_139_51e-01,
                6.189_835_627_658_323_68e-05,
                2.694_745_986_990_679_44e-05,
            ],
            [
                -6.189_911_088_842_041_94e-05,
                9.999_999_976_921_620_79e-01,
                2.800_314_528_295_100_30e-05,
            ],
            [
                -2.694_572_645_905_283_26e-05,
                -2.800_481_324_283_232_71e-05,
                9.999_999_992_448_291_85e-01,
            ],
        ];
        let position =
            ApparentPosition::from_direction(&[1.0, 0.0, 0.0], &rotation, J2000_TRUE_OBLIQUITY);
        let expected = j2000_plus_x_angles();
        let actual = [
            position.ra_deg,
            position.dec_deg,
            position.lon_deg,
            position.lat_deg,
        ];
        for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(
                (a - e).abs() < 1e-7,
                "angle {i} of date: {a} vs expected {e} degrees"
            );
        }
    }

    /// Through the full chain: the synthetic ephemeris holds Earth fixed at
    /// the SSB and the Moon exactly on +x, so the ICRF apparent direction is
    /// +x and the angles of date must equal the anchor decomposition.
    #[test]
    fn apparent_geocentric_matches_the_j2000_anchor() {
        // Coverage straddles t = 0 so the light-time lookback (3.3 s) stays
        // inside the kernel; the body is static at 1e6 km on +x.
        let zero = [0.0, 0.0];
        let x = [1_000_000.0, 0.0];
        let segment = [(-DAY, 2.0 * DAY, 0.0, DAY)];
        let earth = synthetic_type2(
            399,
            0,
            segment[0].0,
            segment[0].1,
            &[(segment[0].2, segment[0].3, &zero, &zero, &zero)],
        );
        let moon = synthetic_type2(
            301,
            0,
            segment[0].0,
            segment[0].1,
            &[(segment[0].2, segment[0].3, &x, &zero, &zero)],
        );
        let ephemeris = Ephemeris::new(vec![earth, moon]);

        let position = apparent_geocentric(&ephemeris, Body::Moon, TdbSeconds(0.0))
            .expect("synthetic bodies resolve");
        let expected = j2000_plus_x_angles();
        let actual = [
            position.ra_deg,
            position.dec_deg,
            position.lon_deg,
            position.lat_deg,
        ];
        for (i, (a, e)) in actual.iter().zip(expected.iter()).enumerate() {
            assert!(
                (a - e).abs() < 1e-7,
                "angle {i} of date: {a} vs expected {e} degrees"
            );
        }
    }
}
