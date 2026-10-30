//! Apparent geocentric positions: where a body appears in the sky from the
//! Earth's center, computed from the barycentric SPK positions.
//!
//! The chain applies, in order:
//!
//! 1. **Light time** ([`light_time`]) — the body is evaluated at the emission
//!    instant whose light reaches the geocenter at the requested time.
//! 2. **Annual aberration** ([`aberration`]) — the geocenter's barycentric
//!    velocity tilts the received direction by up to 20.5″.
//!
//! The frame layer (IAU 2006 precession and IAU 2000B nutation) completes the
//! chain in the next commit; until then the seam reports the aberrated
//! direction in ICRF/J2000 components.

pub(crate) mod aberration;
pub(crate) mod light_time;

use crate::error::EphemerisError;
use crate::spk::{Body, Ephemeris};
use crate::time::TdbSeconds;

/// The aberrated unit direction from the geocenter at `t` toward `body`,
/// evaluated at its light-time-corrected emission instant, in ICRF/J2000
/// components.
///
/// The precession-nutation frame layer rotates this direction onto the true
/// equator and equinox of date in the next commit.
///
/// # Errors
///
/// Returns SPK lookup errors when a body is unknown or the requested instant
/// (including the light-time lookback) falls outside kernel coverage, and the
/// light-time convergence error from [`light_time`].
pub fn apparent_direction_icrf(
    ephemeris: &Ephemeris,
    body: Body,
    t: TdbSeconds,
) -> Result<[f64; 3], EphemerisError> {
    let earth_bary_km = ephemeris.barycentric_position(Body::Earth, t)?;
    let solution =
        light_time::light_time_corrected(ephemeris, body, t, &earth_bary_km)?;
    let earth_velocity_km_s = aberration::earth_velocity_km_s(ephemeris, t)?;
    let unit = unit_vector(solution.geometric_km);
    Ok(aberration::aberrate(unit, &earth_velocity_km_s))
}

/// Scales `v` to unit length. Division by zero is impossible for kernel
/// positions: every ephemeris body's geocentric distance is strictly positive.
fn unit_vector(v: [f64; 3]) -> [f64; 3] {
    let length = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / length, v[1] / length, v[2] / length]
}

#[cfg(test)]
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
        let direction = apparent_direction_icrf(&ephemeris, Body::Moon, TdbSeconds(t))
            .expect("synthetic bodies resolve");
        // The direction is exactly +x: the body is on the x axis, the observer
        // is fixed, and the aberration therefore contributes nothing.
        assert!(
            direction[0] > 0.999_999_999 && direction[1].abs() < 1e-15 && direction[2].abs() < 1e-15,
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
}
