//! Historical-offset table tests for [`geotime::resolve`].
//!
//! Every row pins a known UTC offset for a real historical date, checked
//! against the IANA tzdata history `chrono-tz` carries. A wrong historical
//! offset silently shifts every astrocartography line, so these rows are the
//! accuracy gate for Layer 3.

use chrono::{FixedOffset, NaiveDate, NaiveTime};
use geotime::{OffsetBasis, ResolveError, TimeInput};

struct Row {
    city: &'static str,
    country: &'static str,
    date: NaiveDate,
    offset_seconds: i32,
    basis: OffsetBasis,
}

fn row(
    city: &'static str,
    country: &'static str,
    date: (i32, u32, u32),
    offset_seconds: i32,
    basis: OffsetBasis,
) -> Row {
    Row {
        city,
        country,
        date: NaiveDate::from_ymd_opt(date.0, date.1, date.2)
            .expect("invariant: table dates are valid"),
        offset_seconds,
        basis,
    }
}

fn noon() -> NaiveTime {
    NaiveTime::from_hms_opt(12, 0, 0).expect("invariant: noon is a valid time")
}

#[test]
fn historical_offsets_match_known_values() {
    let table = [
        // US daylight saving before the 1966 Uniform Time Act was a local
        // option; New York observed it every summer, so July is EDT (-4h)
        // and January EST (-5h) in both years.
        row(
            "New York City",
            "US",
            (1955, 7, 15),
            -14_400,
            OffsetBasis::ZoneRule,
        ),
        row(
            "New York City",
            "US",
            (1955, 1, 15),
            -18_000,
            OffsetBasis::ZoneRule,
        ),
        row(
            "New York City",
            "US",
            (1965, 7, 15),
            -14_400,
            OffsetBasis::ZoneRule,
        ),
        // British Standard Time (Oct 1968 - Oct 1971): the UK stayed on
        // UTC+1 through the winter.
        row("London", "GB", (1969, 1, 15), 3_600, OffsetBasis::ZoneRule),
        // The winters on either side of the experiment were back to GMT.
        row("London", "GB", (1966, 1, 15), 0, OffsetBasis::ZoneRule),
        // Moscow adopted annual DST in 1981: +3 became +4 that summer.
        row("Moscow", "RU", (1981, 7, 1), 14_400, OffsetBasis::ZoneRule),
        row("Moscow", "RU", (1980, 7, 1), 10_800, OffsetBasis::ZoneRule),
        // Pre-standard-time births resolve to local mean time, seconds
        // granularity and all: New York's LMT is UTC-4:56:02, London's
        // UTC-0:01:15.
        row(
            "New York City",
            "US",
            (1880, 5, 5),
            -17_762,
            OffsetBasis::LocalMeanTime,
        ),
        row(
            "London",
            "GB",
            (1800, 1, 1),
            -75,
            OffsetBasis::LocalMeanTime,
        ),
    ];

    for case in &table {
        let resolved = geotime::resolve(case.city, case.country, case.date, Some(noon()))
            .unwrap_or_else(|error| panic!("{} must resolve: {error:?}", case.city));
        assert_eq!(
            resolved.offset,
            FixedOffset::east_opt(case.offset_seconds).expect("invariant: table offsets are valid"),
            "{} {} must be UTC{:+03}:{:02}",
            case.city,
            case.date,
            case.offset_seconds / 3600,
            (case.offset_seconds % 3600) / 60,
        );
        assert_eq!(
            resolved.offset_basis, case.basis,
            "{} {}",
            case.city, case.date
        );
    }
}

#[test]
fn unknown_birth_time_resolves_to_noon_and_flags_reduced_accuracy() {
    let date = NaiveDate::from_ymd_opt(1955, 7, 15).expect("invariant: valid date");
    let resolved = geotime::resolve("New York City", "US", date, None).expect("city is bundled");
    assert_eq!(resolved.time, TimeInput::UnknownNoon);
    assert_eq!(resolved.local.time(), noon());
    // Noon on 1955-07-15 in New York is EDT.
    assert_eq!(
        resolved.offset,
        FixedOffset::east_opt(-14_400).expect("invariant: valid offset")
    );
}

#[test]
fn unresolved_city_names_the_closest_matches() {
    let date = NaiveDate::from_ymd_opt(1990, 6, 15).expect("invariant: valid date");
    let error = geotime::resolve("Sprinfield", "US", date, Some(noon()))
        .expect_err("typo must not resolve");
    match error {
        ResolveError::UnknownCity {
            city,
            country,
            suggestions,
        } => {
            assert_eq!(city, "Sprinfield");
            assert_eq!(country, "United States");
            assert_eq!(
                suggestions.first().map(|suggestion| suggestion.name),
                Some("Springfield")
            );
            assert!(suggestions.len() <= 3);
        }
        ResolveError::UnknownCountry { .. } => panic!("country must resolve"),
    }
}

#[test]
fn unresolved_country_names_the_closest_matches() {
    let date = NaiveDate::from_ymd_opt(1990, 6, 15).expect("invariant: valid date");
    let error = geotime::resolve("New York City", "Untied States", date, Some(noon()))
        .expect_err("typo must not resolve");
    match error {
        ResolveError::UnknownCountry { query, suggestions } => {
            assert_eq!(query, "Untied States");
            assert_eq!(
                suggestions.first().map(|suggestion| suggestion.name),
                Some("United States")
            );
        }
        ResolveError::UnknownCity { .. } => panic!("country must fail first"),
    }
}

#[test]
fn same_named_cities_disambiguate_by_country() {
    let date = NaiveDate::from_ymd_opt(1990, 6, 15).expect("invariant: valid date");
    let london = geotime::resolve("London", "GB", date, Some(noon())).expect("city is bundled");
    assert_eq!(london.zone, chrono_tz::Europe::London);
    // The Canadian London keeps its own zone.
    let ontario =
        geotime::resolve("London", "Canada", date, Some(noon())).expect("city is bundled");
    assert_eq!(ontario.zone, chrono_tz::America::Toronto);
}

#[test]
fn country_matches_code_spelling_or_name() {
    let date = NaiveDate::from_ymd_opt(1990, 6, 15).expect("invariant: valid date");
    for spelling in ["US", "USA", "United States", "united states"] {
        let resolved = geotime::resolve("New York City", spelling, date, Some(noon()))
            .expect("spelling must resolve");
        assert_eq!(resolved.country_code, "US", "{spelling}");
    }
}
