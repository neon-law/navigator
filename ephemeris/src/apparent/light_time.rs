//! Light-time correction: a body is rendered where it was when its light
//! left, not where it is when the light arrives.
//!
//! For an arrival instant `t` at the geocenter, the routine finds the emission
//! time `t − τ` whose body position satisfies `|r_body(t − τ) − r_earth(t)| =
//! c·τ`. Planetary light times range from about 1.3 s (Moon) to hours (outer
//! planets); each iteration re-evaluates the SPK position at the candidate
//! emission time and contracts the residual by the ratio of radial velocity
//! to light speed (≈ 1e-4), so convergence takes two to three passes.

use crate::error::EphemerisError;
use crate::spk::{Body, Ephemeris};
use crate::time::TdbSeconds;

/// Speed of light in km/s (exact; the metre is defined by it).
pub(crate) const C_KM_S: f64 = 299_792.458;

/// Convergence tolerance on the light-time solution, in seconds. At the
/// fastest line-of-sight motion in the line set (≈ 40 km/s) the residual
/// displacement is under 0.04 mm — angularly nil.
const CONVERGENCE_TOLERANCE_S: f64 = 1e-9;

/// Iteration cap. Real ephemerides converge in two to three passes; the cap
/// only guards a pathological kernel and is never reached in practice.
const MAX_ITERATIONS: usize = 10;

/// A converged light-time solution.
///
/// Self-consistent by construction: `geometric_km` is evaluated exactly at the
/// emission instant `t − τ`, and its length equals `c·τ` — the emission time
/// is therefore always recoverable as `t − |geometric_km| / c`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct LightTimeSolution {
    /// Geocentric-to-body vector at the emission instant, km, ICRF/J2000.
    pub geometric_km: [f64; 3],
}

/// Iterates the light-time equation for `body` observed from the geocenter at
/// the arrival instant `t`.
///
/// `earth_bary_km` is the Earth's barycentric position at `t` (evaluated once
/// by the caller; the geocenter does not move measurably during a light-time
/// pass). The returned solution is self-consistent: `geometric_km` is
/// evaluated exactly at `emit_time`, and its length equals `c·τ`.
///
/// # Errors
///
/// Returns SPK lookup errors ([`EphemerisError::UnknownBody`],
/// [`EphemerisError::OutsideCoverage`]) when a pass falls outside the
/// kernel's coverage, and [`EphemerisError::LightTimeNoConvergence`] if the
/// iteration cap is reached.
pub(crate) fn light_time_corrected(
    ephemeris: &Ephemeris,
    body: Body,
    t: TdbSeconds,
    earth_bary_km: &[f64; 3],
) -> Result<LightTimeSolution, EphemerisError> {
    // First guess: the light time of the arrival-time geometric distance.
    let arrival = ephemeris.barycentric_position(body, t)?;
    let mut geometric_km = subtract(arrival, *earth_bary_km);
    let mut tau_s = norm(geometric_km) / C_KM_S;

    for _ in 0..MAX_ITERATIONS {
        let emitted = ephemeris.barycentric_position(body, TdbSeconds(t.0 - tau_s))?;
        let candidate = subtract(emitted, *earth_bary_km);
        let candidate_tau_s = norm(candidate) / C_KM_S;
        let moved = (candidate_tau_s - tau_s).abs();
        tau_s = candidate_tau_s;
        geometric_km = candidate;
        if moved <= CONVERGENCE_TOLERANCE_S {
            return Ok(LightTimeSolution { geometric_km });
        }
    }

    Err(EphemerisError::LightTimeNoConvergence {
        naif_id: body.naif_id(),
        iterations: MAX_ITERATIONS,
    })
}

/// Component-wise difference `a − b`.
fn subtract(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

/// Euclidean length of `v`.
fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spk::test_support::synthetic_type2;

    const DAY: f64 = 86_400.0;

    /// Linear-motion x coefficients: the type-2 Chebyshev form over one
    /// segment `[0, span]` evaluates to `c0 + c1·(2t/span − 1)`, which equals
    /// `x0 + v·t` for `c1 = v·span/2` and `c0 = x0 + c1`.
    fn linear_x_coefficients(x0_km: f64, velocity_km_s: f64, span_s: f64) -> [f64; 2] {
        let c1 = velocity_km_s * span_s / 2.0;
        [x0_km + c1, c1]
    }

    /// An ephemeris of two synthetic kernels: Earth fixed at the SSB and the
    /// Moon moving along +x at the given velocity.
    fn moving_body_ephemeris(x0_km: f64, velocity_km_s: f64) -> Ephemeris {
        let half = DAY / 2.0;
        let x = linear_x_coefficients(x0_km, velocity_km_s, DAY);
        // All three axes in a type-2 record carry the same coefficient count;
        // the constant axes get an extra zero Chebyshev term.
        let zero1 = [0.0];
        let zero2 = [0.0, 0.0];
        let earth = synthetic_type2(399, 0, 0.0, DAY, &[(half, half, &zero1, &zero1, &zero1)]);
        let moon = synthetic_type2(301, 0, 0.0, DAY, &[(half, half, &x, &zero2, &zero2)]);
        Ephemeris::new(vec![earth, moon])
    }

    #[test]
    fn static_body_converges_in_one_pass() {
        // Earth fixed at the origin, body fixed at 1e6 km on +x: the light
        // time is exactly the distance over c, and the iteration must land on
        // it immediately.
        let ephemeris = moving_body_ephemeris(1_000_000.0, 0.0);
        let earth = [0.0; 3];
        let t = TdbSeconds(DAY / 2.0);
        let solution =
            light_time_corrected(&ephemeris, Body::Moon, t, &earth).expect("static body converges");
        let expected_tau = 1_000_000.0 / C_KM_S;
        let tau = norm(solution.geometric_km) / C_KM_S;
        assert!((tau - expected_tau).abs() <= 1e-8);
        for (component, expected) in solution.geometric_km.iter().zip([1_000_000.0, 0.0, 0.0]) {
            assert!((component - expected).abs() <= 1e-6);
        }
    }

    #[test]
    fn receding_body_matches_the_closed_form() {
        // A body receding at v has light time τ = (x0 + v·t)/(c + v).
        let (x0, v) = (1_000_000.0_f64, 10.0_f64);
        let ephemeris = moving_body_ephemeris(x0, v);
        let earth = [0.0; 3];
        let t = DAY / 2.0;
        let solution = light_time_corrected(&ephemeris, Body::Moon, TdbSeconds(t), &earth)
            .expect("receding body converges");
        let expected_tau = (x0 + v * t) / (C_KM_S + v);
        let tau = norm(solution.geometric_km) / C_KM_S;
        assert!(
            (tau - expected_tau).abs() <= 1e-9,
            "light time {tau} s vs closed form {expected_tau} s"
        );
        let distance = norm(solution.geometric_km);
        assert!(
            (distance - C_KM_S * expected_tau).abs() <= 1e-6,
            "geometric distance {distance} vs c·τ {}",
            C_KM_S * expected_tau
        );
    }

    #[test]
    fn approaching_body_shortens_the_light_time() {
        // An approaching body (v < 0) is seen closer than it arrives: τ is
        // smaller than the arrival-time distance over c.
        let (x0, v) = (1_000_000.0_f64, -10.0_f64);
        let ephemeris = moving_body_ephemeris(x0, v);
        let earth = [0.0; 3];
        let t = DAY / 2.0;
        let solution = light_time_corrected(&ephemeris, Body::Moon, TdbSeconds(t), &earth)
            .expect("approaching body converges");
        let expected_tau = (x0 + v * t) / (C_KM_S + v);
        let tau = norm(solution.geometric_km) / C_KM_S;
        assert!(
            (tau - expected_tau).abs() <= 1e-9,
            "light time {tau} s vs closed form {expected_tau} s"
        );
        let arrival_tau = (x0 + v * t) / C_KM_S;
        assert!(
            expected_tau > arrival_tau,
            "approaching body was farther at emission, so its light time must exceed \
             the arrival-distance estimate"
        );
    }
}
