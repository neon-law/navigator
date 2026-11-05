//! Independent, explicitly Moshier state/rate checks at a matched TT instant.

use ephemeris::{
    AstrologyChartProvider, AstrologyChartRequest, BirthTime, EphemerisManifest, FilesystemStore,
    LocalBirthTime, DATA_DIR_ENV,
};
use serde::Deserialize;
use std::num::NonZeroUsize;

#[derive(Deserialize)]
struct Reference {
    instant_utc: String,
    jd_tt: f64,
    rows: Vec<Row>,
}
#[derive(Deserialize)]
struct Row {
    body: String,
    source_id: i32,
    returned_flags: i32,
    before_flags: i32,
    after_flags: i32,
    equatorial_flags: i32,
    longitude_deg: f64,
    latitude_deg: f64,
    right_ascension_deg: f64,
    declination_deg: f64,
    distance_au: f64,
    longitude_speed_deg_per_day: f64,
    latitude_speed_deg_per_day: f64,
    range_derivative_au_per_day: f64,
}
fn arc_distance(a: f64, b: f64) -> f64 {
    ((a - b + 180.0).rem_euclid(360.0) - 180.0).abs()
}

#[test]
fn apparent_states_and_signed_rates_agree_with_independent_moshier_values() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("skipping: pinned reference kernels are unavailable");
        return;
    }
    let reference: Reference =
        serde_json::from_str(include_str!("../fixtures/swisseph_state_reference.json"))
            .expect("independent fixture parses");
    let manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../manifest.json")).expect("source manifest parses");
    let provider = AstrologyChartProvider::load(
        &FilesystemStore::from_env().expect("reference source directory"),
        &manifest,
        NonZeroUsize::new(1).expect("positive capacity"),
    )
    .expect("strictly pinned kernels load");
    let request = AstrologyChartRequest::new(
        BirthTime::Exact(LocalBirthTime {
            local: reference
                .instant_utc
                .parse()
                .expect("synthetic reference instant"),
            zone: chrono_tz::UTC,
            offset_seconds: None,
        }),
        0.0,
        0.0,
    );
    let result = provider
        .calculate(&request)
        .expect("reference calculation succeeds");
    let scales = result
        .provenance
        .time_scales
        .as_ref()
        .expect("exact-time scales");
    assert!(
        (scales.julian_day_tt - reference.jd_tt).abs() < 1e-10,
        "reference coordinates must be evaluated at the same TT instant"
    );
    let positions = result.positions.as_ref().expect("exact-time positions");
    for expected in reference.rows {
        let actual = positions
            .iter()
            .find(|position| position.body == expected.body)
            .expect("requested reference body exists");
        assert_reference_state(actual, &expected);
    }
}

fn assert_reference_state(actual: &ephemeris::BodyPosition, expected: &Row) {
    for flags in [
        expected.returned_flags,
        expected.before_flags,
        expected.after_flags,
        expected.equatorial_flags,
    ] {
        assert!(
            flags & 4 != 0 && flags & 2 == 0,
            "actual reference flags must identify Moshier"
        );
    }
    assert_eq!(
        actual.source_id,
        Some(expected.source_id),
        "source target identity"
    );
    for (a, b) in [
        (actual.longitude_deg, expected.longitude_deg),
        (actual.latitude_deg, expected.latitude_deg),
        (actual.right_ascension_deg, expected.right_ascension_deg),
        (actual.declination_deg, expected.declination_deg),
    ] {
        assert!(
            arc_distance(a, b) * 3600.0 < 1.0,
            "apparent direction exceeds one arcsecond"
        );
    }
    for (a, b) in [
        (
            actual.longitude_speed_deg_per_day,
            expected.longitude_speed_deg_per_day,
        ),
        (
            actual.latitude_speed_deg_per_day,
            expected.latitude_speed_deg_per_day,
        ),
    ] {
        assert!(
            (a - b).abs() * 3600.0 < 1.0,
            "signed angular rate exceeds one arcsecond/day"
        );
    }
    let au_km = 149_597_870.7;
    let range = actual.distance_km.expect("physical range exists");
    let reference_range = expected.distance_au * au_km;
    assert!(
        (range - reference_range).abs() / reference_range < 1e-5,
        "range exceeds the Moshier comparison margin"
    );
    let rate = actual
        .distance_speed_km_per_day
        .expect("physical range derivative exists");
    assert!(
        (rate - expected.range_derivative_au_per_day * au_km).abs() < 5.0,
        "range derivative exceeds the matched comparison margin"
    );
}
