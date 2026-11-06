//! Real pinned-source daily reference rows, separate from personal chart persistence.

use chrono::{NaiveDate, Timelike};
use ephemeris::{
    lines::{LineBody, LINE_BODIES},
    AstrologyChartProvider, AstrologyChartRequest, BirthTime, BirthTimeScale, EphemerisManifest,
    EphemerisReferenceTable, FilesystemStore, LocalBirthTime, DATA_DIR_ENV,
};
use std::{collections::BTreeSet, num::NonZeroUsize, path::PathBuf};

fn source() -> Option<(FilesystemStore, PathBuf)> {
    let Some(root) = std::env::var_os(DATA_DIR_ENV).map(PathBuf::from) else {
        assert!(
            std::env::var_os("NAVIGATOR_REQUIRE_REFERENCE_TABLE_KERNELS").is_none(),
            "required pinned DE440 reference source is unavailable"
        );
        eprintln!("skipping reference table integration: pinned DE440 is unavailable");
        return None;
    };
    Some((FilesystemStore::new(&root), root.join("de440.bsp")))
}

fn day(value: &str) -> NaiveDate {
    value.parse().unwrap()
}

#[test]
fn exact_daily_rows_match_all_uncached_provider_fields_and_exclude_private_data() {
    let Some((store, path)) = source() else {
        return;
    };
    let before = std::fs::metadata(&path).unwrap();
    let mut manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../manifest.json")).unwrap();
    manifest.files.retain(|file| file.name == "de440.bsp");
    let provider =
        AstrologyChartProvider::load(&store, &manifest, NonZeroUsize::new(1).unwrap()).unwrap();
    let bodies: Vec<_> = LINE_BODIES
        .into_iter()
        .filter(|body| *body != LineBody::Chiron)
        .collect();
    let mut observed_retrograde = false;
    for date in [
        "1900-01-01",
        "1955-01-01",
        "1971-12-31",
        "1972-01-01",
        "2000-02-29",
        "2016-12-31",
        "2017-01-01",
        "2024-04-03",
        "2049-12-31",
    ] {
        let from = day(date);
        let table =
            EphemerisReferenceTable::calculate(&store, from, from.succ_opt().unwrap()).unwrap();
        assert_eq!(table.rows.len(), 11);
        let mut request = AstrologyChartRequest::new(
            BirthTime::Exact(LocalBirthTime {
                local: from.and_hms_opt(0, 0, 0).unwrap(),
                zone: chrono_tz::UTC,
                offset_seconds: None,
            }),
            0.0,
            0.0,
        );
        request.bodies.clone_from(&bodies);
        let reference = provider.calculate_uncached(&request).unwrap();
        let positions = reference.positions.as_ref().unwrap();
        let expected_time = reference.instant.unwrap().time_scales();
        let mut ids = BTreeSet::new();
        for ((row, expected), body) in table.rows.iter().zip(positions).zip(&bodies) {
            assert_eq!(
                serde_json::to_value(&row.position).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            assert_eq!(row.position.body, body.name());
            assert_eq!(
                row.position.source_id,
                body.spk_body().map(ephemeris::Body::naif_id)
            );
            assert_eq!(row.reference_calendar.date(), from);
            assert_eq!(row.reference_calendar.time().num_seconds_from_midnight(), 0);
            assert_eq!(row.reference_scale, expected_time.reference.scale);
            assert_eq!(row.julian_day_utc, expected_time.julian_day_utc);
            assert_eq!(
                row.model_julian_day_ut1.to_bits(),
                expected_time.model_julian_day_ut1.to_bits()
            );
            assert_eq!(
                row.spk_tdb_seconds_since_j2000.to_bits(),
                reference
                    .instant
                    .unwrap()
                    .epoch()
                    .to_tdb_seconds()
                    .to_bits()
            );
            assert!(ids.insert(row.position.body));
            assert_ne!(row.position.body, "chiron");
            assert!(row.position.longitude_deg >= 0.0 && row.position.longitude_deg < 360.0);
            assert_finite_row(row);
            if *body == LineBody::MeanNode {
                assert!(row.position.source_id.is_none());
                assert!(row.position.distance_km.is_none());
                assert!(row.position.distance_speed_km_per_day.is_none());
            }
            if date == "2024-04-03" && *body == LineBody::Mercury {
                assert!(row.position.longitude_speed_deg_per_day < 0.0);
                observed_retrograde = true;
            }
        }
        assert_eq!(ids.len(), 11);
        assert!(!table.metadata.interpolation_supported);
        assert_eq!(table.metadata.source_manifest.files.len(), 1);
        assert_eq!(table.metadata.source_manifest.files[0].name, "de440.bsp");
        assert_serialization_boundary(&table);
    }
    assert!(observed_retrograde);
    let after = std::fs::metadata(path).unwrap();
    assert_eq!(before.len(), after.len());
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
}

fn assert_finite_row(row: &ephemeris::EphemerisReferenceRow) {
    for value in [
        row.position.longitude_deg,
        row.position.latitude_deg,
        row.position.right_ascension_deg,
        row.position.declination_deg,
        row.position.longitude_speed_deg_per_day,
        row.position.latitude_speed_deg_per_day,
        row.spk_tdb_seconds_since_j2000,
        row.model_julian_day_ut1,
        row.delta_t_seconds,
    ]
    .into_iter()
    .chain(row.position.distance_km)
    .chain(row.position.distance_speed_km_per_day)
    {
        assert!(value.is_finite());
    }
}

fn assert_serialization_boundary(table: &EphemerisReferenceTable) {
    let json = serde_json::to_value(table).unwrap();
    assert_no_private_keys(&json);
    let encoded = serde_json::to_vec(table).unwrap();
    let decoded: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    for (row, decoded) in table.rows.iter().zip(decoded["rows"].as_array().unwrap()) {
        assert_eq!(
            decoded["tt_nanoseconds_since_j2000"].as_i64(),
            Some(row.tt_nanoseconds_since_j2000)
        );
        assert_eq!(
            decoded["tdb_nanoseconds_since_j2000"].as_i64(),
            Some(row.tdb_nanoseconds_since_j2000)
        );
        if row.reference_scale == BirthTimeScale::Ut1 {
            assert!(decoded["julian_day_utc"].is_null());
        }
        if row.position.body == "mean_node" {
            assert!(decoded["distance_km"].is_null());
        }
    }
}

fn assert_no_private_keys(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                assert!(![
                    "request",
                    "response",
                    "birth_time",
                    "zone",
                    "tzdb_version",
                    "consent",
                    "person_id",
                    "project_id",
                    "matter_id",
                    "cache",
                    "angles",
                    "houses"
                ]
                .contains(&key.as_str()));
                assert_no_private_keys(value);
            }
        }
        serde_json::Value::Array(array) => {
            for value in array {
                assert_no_private_keys(value);
            }
        }
        _ => {}
    }
}

#[test]
fn complete_leap_year_batches_are_deterministic_and_preserve_wrapped_longitudes() {
    let Some((store, _)) = source() else {
        return;
    };
    let first =
        EphemerisReferenceTable::calculate(&store, day("2000-01-01"), day("2001-01-01")).unwrap();
    let second =
        EphemerisReferenceTable::calculate(&store, day("2000-01-01"), day("2001-01-01")).unwrap();
    assert_eq!(first.rows.len(), 366 * 11);
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap()
    );
    let dates: BTreeSet<_> = first
        .rows
        .iter()
        .map(|row| row.reference_calendar.date())
        .collect();
    assert_eq!(dates.len(), 366);
    assert!(dates.contains(&day("2000-02-29")));
    let leap =
        EphemerisReferenceTable::calculate(&store, day("2016-12-31"), day("2017-01-02")).unwrap();
    assert_eq!(leap.rows.len(), 22);
    assert_eq!(leap.rows[11].reference_calendar.date(), day("2017-01-01"));
    assert_eq!(
        leap.rows[11].tt_nanoseconds_since_j2000 - leap.rows[0].tt_nanoseconds_since_j2000,
        86_401_000_000_000
    );
    let transition =
        EphemerisReferenceTable::calculate(&store, day("1971-12-31"), day("1972-01-02")).unwrap();
    assert_eq!(transition.rows[0].reference_scale, BirthTimeScale::Ut1);
    assert!(transition.rows[0].julian_day_utc.is_none());
    assert_eq!(transition.rows[11].reference_scale, BirthTimeScale::Utc);
    assert!(transition.rows[11].julian_day_utc.is_some());
    let wrap =
        EphemerisReferenceTable::calculate(&store, day("2024-03-19"), day("2024-03-22")).unwrap();
    let sun: Vec<_> = wrap
        .rows
        .iter()
        .filter(|row| row.position.body == "sun")
        .collect();
    assert!(sun
        .windows(2)
        .any(|pair| pair[1].position.longitude_deg < pair[0].position.longitude_deg));
    assert!(sun
        .iter()
        .all(|row| row.position.longitude_speed_deg_per_day > 0.9
            && row.position.longitude_speed_deg_per_day < 1.1));
}
