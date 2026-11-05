//! Tropical angles and house cusps from the local sidereal angle.

use crate::{ChartError, HouseSystem};
use serde::Serialize;

/// Tropical ecliptic longitudes of local angles and optional house cusps.
/// These are chart longitudes, separate from geographic line coordinates.
#[derive(Clone, Serialize)]
pub struct ChartAngles {
    /// Ecliptic longitude rising on the eastern horizon.
    pub ascendant_deg: f64,
    /// Ecliptic longitude on the upper meridian.
    pub midheaven_deg: f64,
    /// Opposite the ascendant.
    pub descendant_deg: f64,
    /// Opposite the midheaven.
    pub imum_coeli_deg: f64,
    /// Twelve ecliptic cusp longitudes in house order, when requested.
    pub house_cusps_deg: Option<[f64; 12]>,
}

/// Intersect the ecliptic plane with the local horizon and meridian.
/// ARMC is local apparent sidereal time in degrees; obliquity is true-of-date.
/// A geographic pole or coincident horizon/ecliptic has no unique rising angle.
pub(crate) fn chart_angles(
    armc_deg: f64,
    latitude_deg: f64,
    obliquity_deg: f64,
    houses: &HouseSystem,
) -> Result<Option<ChartAngles>, ChartError> {
    if !armc_deg.is_finite()
        || !latitude_deg.is_finite()
        || !obliquity_deg.is_finite()
        || !(-90.0..=90.0).contains(&latitude_deg)
        || !(0.0..90.0).contains(&obliquity_deg)
    {
        return Err(ChartError::InvalidGeometry);
    }
    if *houses == HouseSystem::Placidus {
        return Err(ChartError::UnsupportedSetting("house system"));
    }
    if latitude_deg.abs() >= 90.0 {
        return Ok(None);
    }
    let (sin_theta, cos_theta) = armc_deg.rem_euclid(360.0).to_radians().sin_cos();
    let (sin_phi, cos_phi) = latitude_deg.to_radians().sin_cos();
    let (sin_eps, cos_eps) = obliquity_deg.to_radians().sin_cos();
    // Cross product of the ecliptic normal and zenith, expressed on the ecliptic.
    let mut x = -sin_eps * sin_phi - cos_eps * cos_phi * sin_theta;
    let mut y = cos_phi * cos_theta;
    if x.hypot(y) < 1e-12 {
        return Ok(None);
    }
    // Choose the intersection whose altitude increases with Earth rotation.
    if cos_eps * cos_phi + sin_eps * sin_phi * sin_theta < 0.0 {
        x = -x;
        y = -y;
    }
    let ascendant_deg = y.atan2(x).to_degrees().rem_euclid(360.0);
    let midheaven_deg = sin_theta
        .atan2(cos_theta * cos_eps)
        .to_degrees()
        .rem_euclid(360.0);
    let house_cusps_deg = match houses {
        HouseSystem::None => None,
        HouseSystem::Equal | HouseSystem::WholeSign => {
            let start = if *houses == HouseSystem::WholeSign {
                (ascendant_deg / 30.0).floor() * 30.0
            } else {
                ascendant_deg
            };
            let mut cusps = [0.0; 12];
            for (index, cusp) in cusps.iter_mut().enumerate() {
                let step = u32::try_from(index).map_err(|_| ChartError::InvalidGeometry)?;
                *cusp = (start + f64::from(step) * 30.0).rem_euclid(360.0);
            }
            Some(cusps)
        }
        HouseSystem::Placidus => return Err(ChartError::UnsupportedSetting("house system")),
    };
    Ok(Some(ChartAngles {
        ascendant_deg,
        midheaven_deg,
        descendant_deg: (ascendant_deg + 180.0).rem_euclid(360.0),
        imum_coeli_deg: (midheaven_deg + 180.0).rem_euclid(360.0),
        house_cusps_deg,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HouseSystem;
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct Table {
        obliquity_deg: f64,
        rows: Vec<Row>,
    }
    #[derive(Deserialize)]
    struct Row {
        armc_deg: f64,
        latitude_deg: f64,
        house_system: String,
        ascendant_deg: f64,
        midheaven_deg: f64,
        cusps_deg: [f64; 12],
    }
    fn difference(a: f64, b: f64) -> f64 {
        (a - b + 180.0).rem_euclid(360.0) - 180.0
    }

    #[test]
    fn geometry_matches_independent_swiss_vectors_with_identical_inputs() {
        let table: Table =
            serde_json::from_str(include_str!("../fixtures/swisseph_house_reference.json"))
                .expect("valid independent reference table");
        assert!(
            !table.rows.is_empty(),
            "reference measurements must be present"
        );
        for (index, row) in table.rows.iter().enumerate() {
            let system = match row.house_system.as_str() {
                "equal" => HouseSystem::Equal,
                "whole_sign" => HouseSystem::WholeSign,
                _ => panic!("unsupported reference convention"),
            };
            let computed =
                chart_angles(row.armc_deg, row.latitude_deg, table.obliquity_deg, &system)
                    .expect("valid reference inputs")
                    .expect("nonpolar reference");
            assert!(
                difference(computed.ascendant_deg, row.ascendant_deg).abs() < 1e-8,
                "ascendant differs at reference row {index}"
            );
            assert!(
                difference(computed.midheaven_deg, row.midheaven_deg).abs() < 1e-8,
                "midheaven differs at reference row {index}"
            );
            let cusps = computed.house_cusps_deg.expect("requested house cusps");
            for (actual, expected) in cusps.iter().zip(row.cusps_deg) {
                assert!(
                    difference(*actual, expected).abs() < 1e-8,
                    "house cusp differs at reference row {index}"
                );
            }
        }
    }

    #[test]
    fn polar_locations_do_not_invent_a_rising_angle() {
        for latitude in [-90.0, 90.0] {
            assert!(
                chart_angles(37.0, latitude, 23.439, &HouseSystem::Equal)
                    .expect("finite polar input")
                    .is_none(),
                "a geographic pole has no unique rising angle"
            );
        }
    }
}
