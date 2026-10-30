//! Annual aberration: the observer's barycentric motion tilts the received
//! light direction toward the direction of motion by up to 20.5″.
//!
//! First-order special-relativistic composition: for a unit incoming direction
//! `u` and observer velocity β = v/c, the observed direction is
//! `u + β − (u·β)u` — the transverse velocity component tilts the direction,
//! the radial component only scales it. The neglected second-order term is
//! β² ≈ 1e-8 rad ≈ 0.002″, 500× below the 1″ verification gate.

use crate::error::EphemerisError;
use crate::spk::{Body, Ephemeris};
use crate::time::TdbSeconds;

use super::light_time::C_KM_S;

/// Half-width of the central-difference window for Earth's barycentric
/// velocity, in seconds. Truncation error scales as h²·|jerk|/6 ≈ 2e-9 km/s
/// for Earth at h = 100 s, and round-off as ε·|r|/h ≈ 3e-10 km/s — both
/// angularly nil. The window also stays far inside every DE440 interpolation
/// interval, so no segment boundary contributes curvature.
pub(crate) const VELOCITY_STEP_S: f64 = 100.0;

/// Earth's barycentric velocity at `t`, km/s, by central differences of the
/// SPK positions at `t ± h`.
///
/// # Errors
///
/// Returns SPK lookup errors when either end of the difference window falls
/// outside the kernel coverage.
pub(crate) fn earth_velocity_km_s(
    ephemeris: &Ephemeris,
    t: TdbSeconds,
) -> Result<[f64; 3], EphemerisError> {
    let later = ephemeris.barycentric_position(Body::Earth, TdbSeconds(t.0 + VELOCITY_STEP_S))?;
    let earlier = ephemeris.barycentric_position(Body::Earth, TdbSeconds(t.0 - VELOCITY_STEP_S))?;
    Ok([
        (later[0] - earlier[0]) / (2.0 * VELOCITY_STEP_S),
        (later[1] - earlier[1]) / (2.0 * VELOCITY_STEP_S),
        (later[2] - earlier[2]) / (2.0 * VELOCITY_STEP_S),
    ])
}

/// Applies first-order stellar aberration to the unit incoming direction `u`.
///
/// Returns the corrected direction *without* renormalizing: the caller
/// normalizes once at the end of the apparent-position chain.
#[must_use = "the aberrated direction is returned"]
pub(crate) fn aberrate(u: [f64; 3], earth_velocity_km_s: &[f64; 3]) -> [f64; 3] {
    let beta = [
        earth_velocity_km_s[0] / C_KM_S,
        earth_velocity_km_s[1] / C_KM_S,
        earth_velocity_km_s[2] / C_KM_S,
    ];
    let projection = u[0] * beta[0] + u[1] * beta[1] + u[2] * beta[2];
    [
        u[0] + beta[0] - projection * u[0],
        u[1] + beta[1] - projection * u[1],
        u[2] + beta[2] - projection * u[2],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const EARTH_SPEED_KM_S: f64 = 29.78;

    /// The angle between the incoming and aberrated directions, radians.
    fn deflection(u: [f64; 3], v: [f64; 3]) -> f64 {
        let nu = norm(u);
        let nv = norm(v);
        let cos = dot(u, v) / (nu * nv);
        cos.clamp(-1.0, 1.0).acos()
    }

    fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
        a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
    }

    fn norm(a: [f64; 3]) -> f64 {
        (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt()
    }

    #[test]
    fn transverse_motion_tilts_by_the_classic_angle() {
        // Light arriving along +x while the observer moves along +y: the
        // deflection is β = v/c, the classic ≈ 20.5″ constant.
        let u = [1.0, 0.0, 0.0];
        let velocity = [0.0, EARTH_SPEED_KM_S, 0.0];
        let corrected = aberrate(u, &velocity);
        let angle = deflection(u, corrected);
        let expected = EARTH_SPEED_KM_S / C_KM_S;
        assert!(
            (angle - expected).abs() < 1e-12,
            "deflection {angle} rad vs expected β {expected} rad"
        );
    }

    #[test]
    fn radial_motion_does_not_tilt_to_first_order() {
        // Moving directly along the line of sight scales the direction but
        // does not rotate it at first order.
        let u = [0.0, 1.0, 0.0];
        let velocity = [0.0, EARTH_SPEED_KM_S, 0.0];
        let corrected = aberrate(u, &velocity);
        let angle = deflection(u, corrected);
        assert!(
            angle < 1e-12,
            "radial motion must not tilt the direction, got {angle} rad"
        );
    }

    #[test]
    fn tilt_is_toward_the_motion() {
        // The corrected direction must gain a component along the velocity.
        let u = [1.0, 0.0, 0.0];
        let velocity = [0.0, EARTH_SPEED_KM_S, 0.0];
        let corrected = aberrate(u, &velocity);
        assert!(
            corrected[1] > 0.0,
            "the y component must tilt toward +y, got {}",
            corrected[1]
        );
        let magnitude = norm(corrected);
        assert!(
            (magnitude - 1.0).abs() < 2.0e-4,
            "the correction is O(β²); magnitude {magnitude} must stay ≈ 1"
        );
    }
}
