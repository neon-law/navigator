//! Explicit calculation settings and validated chart requests.

use crate::{BirthTime, BirthTimeError, ResolvedBirthTime};
use chrono::Datelike;
use serde::Serialize;

/// Zodiac reference used for ecliptic longitude.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Zodiac {
    /// Longitude from the equinox of date.
    #[default]
    Tropical,
    /// A requested sidereal convention; unsupported by this provider.
    Sidereal {
        /// Explicit sidereal offset model.
        ayanamsa: Ayanamsa,
    },
}

/// A named sidereal offset, separate from tropical longitude.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ayanamsa {
    /// Lahiri convention; not a supported calculation mode.
    Lahiri,
}

/// Observer origin, independent of the coordinate frame.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObserverOrigin {
    /// Earth's center.
    #[default]
    Geocentric,
    /// Surface observer; unsupported by this provider.
    Topocentric,
}

/// Lunar-node model; a node is not a physical body or fixed star.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NorthNode {
    /// IERS 2003 mean ascending node.
    #[default]
    Mean,
    /// Osculating node; unsupported by this provider.
    True,
}

/// Atmospheric correction policy.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AtmosphericRefraction {
    /// Vacuum directions, with no atmospheric refraction.
    #[default]
    None,
    /// Standard atmosphere; unsupported by this geocentric provider.
    Standard,
}

/// Frame of the returned apparent equatorial and ecliptic directions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateFrame {
    /// True equator and ecliptic of date, IAU 2006/2000B.
    #[default]
    TrueOfDate,
    /// Raw ICRF/J2000 directions; unsupported for this apparent chart API.
    IcrfJ2000,
}

/// Named house construction; unavailable calculations are never substituted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HouseSystem {
    /// Do not request houses.
    #[default]
    None,
    /// Thirty-degree houses starting at the ascendant.
    Equal,
    /// Thirty-degree houses starting at the ascendant's zodiac sign.
    WholeSign,
    /// Placidus; unsupported by this provider.
    Placidus,
}

/// Explicit conventions recorded with every calculation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ChartSettings {
    /// Tropical or explicitly named sidereal longitude.
    pub zodiac: Zodiac,
    /// Observer origin.
    pub origin: ObserverOrigin,
    /// Mean or true ascending lunar node.
    pub north_node: NorthNode,
    /// Atmospheric refraction policy.
    pub refraction: AtmosphericRefraction,
    /// Returned coordinate frame.
    pub frame: CoordinateFrame,
    /// Requested house construction.
    pub houses: HouseSystem,
}

/// Private chart input. Geographic coordinates are east-positive WGS84 degrees.
#[derive(Clone)]
pub struct AstrologyChartRequest {
    /// Original civil time, zone, and precision.
    pub birth_time: BirthTime,
    /// Longitude, east-positive, in [-180, 180].
    pub longitude_deg: f64,
    /// Latitude in [-90, 90].
    pub latitude_deg: f64,
    /// Requested bodies in output order; an empty selection requests angles only.
    pub bodies: Vec<crate::lines::LineBody>,
    /// Explicit calculation conventions.
    pub settings: ChartSettings,
}

/// Calculation errors carry no personal birth inputs or derived positions.
#[derive(Debug, thiserror::Error)]
pub enum ChartError {
    /// A request repeats bodies or exceeds the declared inventory.
    #[error("invalid body selection")]
    InvalidBodySelection,
    /// The pinned numerical model cannot produce a finite result.
    #[error("ephemeris calculation is unavailable for this request")]
    CalculationFailed,
    /// The private in-memory cache cannot be accessed safely.
    #[error("calculation cache is unavailable")]
    CacheUnavailable,
    /// Canonical calculation identity could not be encoded.
    #[error("calculation identity cannot be encoded")]
    SerializationFailed,
    /// A numerical geometry input or result is unsupported or non-finite.
    #[error("chart geometry cannot be calculated from these inputs")]
    InvalidGeometry,
    /// Invalid finite-coordinate bounds.
    #[error("location must have finite WGS84 longitude and latitude")]
    InvalidLocation,
    /// A convention cannot be fulfilled by the selected provider.
    #[error("unsupported calculation setting: {0}")]
    UnsupportedSetting(&'static str),
    /// The initial validation domain is 1900-01-01 through 2049-12-31 universal civil time.
    #[error("time is outside the validated 1900-2050 civil interval")]
    OutsideValidatedRange,
    /// Civil-time ambiguity, gap, offset, or leap-second semantics.
    #[error(transparent)]
    BirthTime(#[from] BirthTimeError),
}

impl AstrologyChartRequest {
    /// Request the declared initial body inventory with explicit civil input.
    #[must_use]
    pub fn new(birth_time: BirthTime, longitude_deg: f64, latitude_deg: f64) -> Self {
        Self {
            birth_time,
            longitude_deg,
            latitude_deg,
            bodies: crate::lines::LINE_BODIES.to_vec(),
            settings: ChartSettings::default(),
        }
    }

    /// Validate coordinates, conventions, coverage, and the original civil clock.
    /// The validation window is [1900-01-01, 2050-01-01) in the normalized civil label.
    pub fn resolve(&self) -> Result<ResolvedBirthTime, ChartError> {
        if !self.longitude_deg.is_finite()
            || !self.latitude_deg.is_finite()
            || !(-180.0..=180.0).contains(&self.longitude_deg)
            || !(-90.0..=90.0).contains(&self.latitude_deg)
        {
            return Err(ChartError::InvalidLocation);
        }
        if self.bodies.len() > crate::lines::LINE_BODIES.len()
            || self
                .bodies
                .iter()
                .enumerate()
                .any(|(index, body)| self.bodies[..index].contains(body))
        {
            return Err(ChartError::InvalidBodySelection);
        }
        self.settings.validate()?;
        let resolved = self.birth_time.resolve()?;
        if let Some(reference) = resolved.reference() {
            let calendar = reference.calendar;
            if !validated_year(calendar.year()) {
                return Err(ChartError::OutsideValidatedRange);
            }
            if let BirthTime::Approximate {
                uncertainty_seconds,
                ..
            } = resolved.original()
            {
                let uncertainty = chrono::Duration::seconds(i64::from(uncertainty_seconds.get()));
                if [
                    calendar.checked_sub_signed(uncertainty),
                    calendar.checked_add_signed(uncertainty),
                ]
                .into_iter()
                .any(|limit| limit.is_none_or(|limit| !validated_year(limit.year())))
                {
                    return Err(ChartError::OutsideValidatedRange);
                }
            }
        } else if let BirthTime::Unknown { date, .. } = resolved.original() {
            if !validated_year(date.year()) {
                return Err(ChartError::OutsideValidatedRange);
            }
        }
        Ok(resolved)
    }
}

fn validated_year(year: i32) -> bool {
    (1900..2050).contains(&year)
}

impl ChartSettings {
    fn validate(&self) -> Result<(), ChartError> {
        if self.zodiac != Zodiac::Tropical {
            return Err(ChartError::UnsupportedSetting("zodiac/ayanamsa"));
        }
        if self.origin != ObserverOrigin::Geocentric {
            return Err(ChartError::UnsupportedSetting("observer origin"));
        }
        if self.north_node != NorthNode::Mean {
            return Err(ChartError::UnsupportedSetting("north node"));
        }
        if self.refraction != AtmosphericRefraction::None {
            return Err(ChartError::UnsupportedSetting("refraction"));
        }
        if self.frame != CoordinateFrame::TrueOfDate {
            return Err(ChartError::UnsupportedSetting("coordinate frame"));
        }
        if self.houses == HouseSystem::Placidus {
            return Err(ChartError::UnsupportedSetting("house system"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BirthTime, LocalBirthTime};
    use chrono::NaiveDate;
    use std::num::NonZeroU32;

    fn request(year: i32) -> AstrologyChartRequest {
        AstrologyChartRequest {
            birth_time: BirthTime::Exact(LocalBirthTime {
                local: NaiveDate::from_ymd_opt(year, 1, 1)
                    .and_then(|date| date.and_hms_opt(12, 0, 0))
                    .expect("synthetic valid time"),
                zone: chrono_tz::UTC,
                offset_seconds: None,
            }),
            longitude_deg: 0.0,
            latitude_deg: 0.0,
            bodies: crate::lines::LINE_BODIES.to_vec(),
            settings: ChartSettings::default(),
        }
    }

    #[test]
    fn unsupported_conventions_are_rejected_instead_of_falling_back() {
        let unsupported = [
            ChartSettings {
                zodiac: Zodiac::Sidereal {
                    ayanamsa: Ayanamsa::Lahiri,
                },
                ..ChartSettings::default()
            },
            ChartSettings {
                origin: ObserverOrigin::Topocentric,
                ..ChartSettings::default()
            },
            ChartSettings {
                north_node: NorthNode::True,
                ..ChartSettings::default()
            },
            ChartSettings {
                refraction: AtmosphericRefraction::Standard,
                ..ChartSettings::default()
            },
            ChartSettings {
                houses: HouseSystem::Placidus,
                ..ChartSettings::default()
            },
            ChartSettings {
                frame: CoordinateFrame::IcrfJ2000,
                ..ChartSettings::default()
            },
        ];
        for settings in unsupported {
            let mut input = request(2000);
            input.settings = settings;
            assert!(
                matches!(input.resolve(), Err(ChartError::UnsupportedSetting(_))),
                "unsupported settings must never be silently replaced"
            );
        }
    }

    #[test]
    fn nonfinite_and_out_of_range_locations_are_rejected() {
        for (longitude, latitude) in [
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
            (180.01, 0.0),
            (-180.01, 0.0),
            (0.0, 90.01),
            (0.0, -90.01),
        ] {
            let mut input = request(2000);
            input.longitude_deg = longitude;
            input.latitude_deg = latitude;
            assert!(
                matches!(input.resolve(), Err(ChartError::InvalidLocation)),
                "invalid geographic coordinates must not enter a calculation"
            );
        }
    }

    #[test]
    fn the_validated_window_is_1900_through_2049_universal_civil_time() {
        for year in [1899, 2050] {
            assert!(
                matches!(
                    request(year).resolve(),
                    Err(ChartError::OutsideValidatedRange)
                ),
                "dates outside the declared initial range must be rejected"
            );
        }
        for year in [1900, 2049] {
            assert!(
                request(year).resolve().is_ok(),
                "dates inside the declared range must be accepted"
            );
        }
    }

    #[test]
    fn uncertainty_must_not_extend_beyond_the_validated_window() {
        let mut input = request(1900);
        input.birth_time = BirthTime::Approximate {
            local: LocalBirthTime {
                local: NaiveDate::from_ymd_opt(1900, 1, 1)
                    .and_then(|date| date.and_hms_opt(0, 0, 0))
                    .expect("synthetic boundary time"),
                zone: chrono_tz::UTC,
                offset_seconds: None,
            },
            uncertainty_seconds: NonZeroU32::new(60).expect("positive uncertainty"),
        };
        assert!(
            matches!(input.resolve(), Err(ChartError::OutsideValidatedRange)),
            "a midpoint inside coverage must not hide uncertainty outside coverage"
        );
    }
}
