#![allow(clippy::doc_markdown)]
//! Offline, deterministic geocoding and time resolution.
//!
//! Given a city, a country, and a birth date, [`resolve`] returns the city's
//! coordinates, its IANA zone, and the UTC offset actually in effect on that
//! historical date — including pre-standard-time births, where the offset is
//! local mean time and flagged as such. When the birth time is unknown, noon
//! local is used and the result is flagged as reduced accuracy.
//!
//! The city index is bundled from the GeoNames free database (CC BY 4.0, see
//! `data/SOURCE.md`): no API keys, no network, no per-lookup cost, and the
//! same input always produces the same answer.

mod index;
mod offset;
mod suggest;

use chrono::{FixedOffset, NaiveDate, NaiveDateTime, NaiveTime};

pub use index::{City, CitySuggestion, Country, CountrySuggestion};
pub use offset::OffsetBasis;

/// The birth time as the visitor entered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInput {
    /// A wall-clock birth time was given.
    Known(NaiveTime),
    /// Birth time unknown — noon local is used, a reduced-accuracy reading.
    UnknownNoon,
}

impl TimeInput {
    /// The wall-clock moment this input stands for.
    #[must_use]
    pub fn naive_time(self) -> NaiveTime {
        match self {
            Self::Known(time) => time,
            Self::UnknownNoon => {
                NaiveTime::from_hms_opt(12, 0, 0).expect("invariant: noon is a valid time")
            }
        }
    }
}

/// A resolved birth place and moment.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoTime {
    /// City name as recorded in the bundled index.
    pub city: &'static str,
    /// ISO 3166-1 alpha-2 country code.
    pub country_code: &'static str,
    pub latitude: f64,
    pub longitude: f64,
    pub zone: chrono_tz::Tz,
    /// Local wall-clock birth moment — the given time, or noon when unknown.
    pub local: NaiveDateTime,
    /// The UTC offset actually in effect at `local`.
    pub offset: FixedOffset,
    /// How `offset` was determined for this date.
    pub offset_basis: OffsetBasis,
    /// Whether the birth time was given or assumed at noon.
    pub time: TimeInput,
}

/// Why a resolution failed, with closest-match suggestions for the visitor to
/// refine their entry against.
#[derive(Debug, Clone, PartialEq)]
pub enum ResolveError {
    /// No country matched the query.
    UnknownCountry {
        query: String,
        suggestions: Vec<CountrySuggestion>,
    },
    /// The country matched but no city in it did.
    UnknownCity {
        city: String,
        country: String,
        suggestions: Vec<CitySuggestion>,
    },
}

/// Resolve a birth place and date to coordinates and a historical UTC offset.
///
/// `birth_time` of `None` means the birth time is unknown: noon local is used
/// and the returned `GeoTime::time` is [`TimeInput::UnknownNoon`], so callers
/// can label the reading as reduced accuracy.
///
/// When several cities share a name in one country the most populous wins;
/// the typeahead layer disambiguates before this is called.
pub fn resolve(
    city: &str,
    country: &str,
    date: NaiveDate,
    birth_time: Option<NaiveTime>,
) -> Result<GeoTime, ResolveError> {
    let index = index::global();
    let matched_country = index
        .country(country)
        .ok_or_else(|| ResolveError::UnknownCountry {
            query: country.to_owned(),
            suggestions: index.closest_countries(country, SUGGESTION_COUNT),
        })?;
    let found =
        index
            .city(city, matched_country.iso2)
            .ok_or_else(|| ResolveError::UnknownCity {
                city: city.to_owned(),
                country: matched_country.name.to_owned(),
                suggestions: index.closest_cities(
                    city,
                    Some(matched_country.iso2),
                    SUGGESTION_COUNT,
                ),
            })?;

    let time = birth_time.map_or(TimeInput::UnknownNoon, TimeInput::Known);
    let local = NaiveDateTime::new(date, time.naive_time());
    let (offset, offset_basis) = offset::offset_for(found.zone, date, time.naive_time());

    Ok(GeoTime {
        city: found.name,
        country_code: matched_country.iso2,
        latitude: found.latitude,
        longitude: found.longitude,
        zone: found.zone,
        local,
        offset,
        offset_basis,
        time,
    })
}

const SUGGESTION_COUNT: usize = 3;

/// Every country in the bundled index, in its bundled order — the page layer
/// uses these as the birth-form country options. The list is static, so it
/// carries no visitor data and can ride the wire or render into a select.
pub fn countries() -> Vec<Country> {
    index::global().countries.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_every_bundled_country() {
        let countries = countries();
        assert!(countries.len() > 200, "index bundles {countries} countries");
        assert!(countries
            .iter()
            .any(|country| country.iso2 == "US" && country.name == "United States"));
        assert!(countries.iter().all(|country| !country.name.is_empty()));
    }

    #[test]
    fn resolves_a_present_day_birth() {
        let date = NaiveDate::from_ymd_opt(1990, 6, 15).expect("invariant: valid date");
        let time = NaiveTime::from_hms_opt(10, 30, 0).expect("invariant: valid time");
        let resolved = resolve("New York City", "US", date, Some(time)).expect("city is bundled");
        assert_eq!(resolved.country_code, "US");
        assert!((resolved.latitude - 40.714_27).abs() < 1e-5);
        assert!((resolved.longitude - (-74.005_97)).abs() < 1e-5);
        assert_eq!(resolved.zone, chrono_tz::America::New_York);
        assert_eq!(resolved.time, TimeInput::Known(time));
    }
}
