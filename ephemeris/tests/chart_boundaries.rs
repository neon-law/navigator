//! Exact source coverage and complete-chart behavior at the declared universal civil boundaries.

use ephemeris::{
    AstrologyChartProvider, AstrologyChartRequest, BirthTime, EphemerisManifest, FilesystemStore,
    LocalBirthTime, DATA_DIR_ENV,
};
use std::num::NonZeroUsize;

#[test]
fn full_inventory_is_available_at_both_validated_civil_boundaries() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("skipping: pinned reference kernels are unavailable");
        return;
    }
    let manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../manifest.json")).expect("valid manifest");
    let provider = AstrologyChartProvider::load(
        &FilesystemStore::from_env().expect("reference source directory"),
        &manifest,
        NonZeroUsize::new(1).expect("positive capacity"),
    )
    .expect("exact pinned kernels load");
    for civil in ["1900-01-01T00:00:00", "2049-12-31T23:59:59.999999999"] {
        let input = AstrologyChartRequest::new(
            BirthTime::Exact(LocalBirthTime {
                local: civil.parse().expect("synthetic boundary date"),
                zone: chrono_tz::UTC,
                offset_seconds: None,
            }),
            0.0,
            0.0,
        );
        assert!(
            provider.calculate(&input).is_ok(),
            "declared universal civil boundaries need source coverage for light time and centered rates"
        );
    }
}

#[allow(clippy::cast_precision_loss)] // Scientific source dates are far inside the exact f64 integer range.
fn source_date_seconds(date: &str) -> f64 {
    let midnight = date
        .parse::<chrono::NaiveDate>()
        .expect("source coverage date")
        .and_hms_opt(0, 0, 0)
        .expect("source midnight");
    let j2000 = "2000-01-01T12:00:00"
        .parse::<chrono::NaiveDateTime>()
        .expect("TDB reference date");
    midnight.signed_duration_since(j2000).num_seconds() as f64
}

#[test]
#[allow(clippy::float_cmp)] // Both source endpoints and TDB midnights are exact integer seconds.
fn declared_source_dates_match_actual_spk_segment_envelopes() {
    use ephemeris::{EphemerisStore, SpkFile};
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("skipping: pinned reference kernels are unavailable");
        return;
    }
    let manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../manifest.json")).expect("valid manifest");
    let store = FilesystemStore::from_env().expect("reference source directory");
    for file in manifest.files {
        let kernel =
            SpkFile::from_bytes(store.read(&file.name).expect("source readable"), file.name)
                .expect("source SPK parses");
        let start = kernel
            .segments()
            .iter()
            .map(|segment| segment.start)
            .reduce(f64::min)
            .expect("source contains segments");
        let end = kernel
            .segments()
            .iter()
            .map(|segment| segment.end)
            .reduce(f64::max)
            .expect("source contains segments");
        assert!(
            start == source_date_seconds(&file.coverage_start)
                && end == source_date_seconds(&file.coverage_end),
            "source provenance dates must describe actual segment coverage"
        );
    }
}
