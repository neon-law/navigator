//! Fixture-table validation against the checked-in Horizons reference.
//!
//! The reference table ([`REFERENCE_JSON`]) holds JPL Horizons geometric
//! barycentric cartesian states (ICRF, km) for every DE440 body the crate
//! resolves plus Chiron, on a 10-year grid from 1900 to 2050, generated from
//! the Horizons API on 2026-10-30; the JSON's own header carries the full
//! provenance. Horizons serves the DE441 solution, which JPL documents as
//! agreeing with DE440 within about a meter for planets across DE440's
//! coverage and about ten meters for the Moon a century from present; the
//! per-body tolerances in the table carry margin above that measured
//! agreement.
//!
//! The interpolation test needs the real kernel files, which stay outside the
//! repository (see [`ephemeris::DATA_DIR_ENV`]): point the variable at a
//! directory containing `de440.bsp` and `chiron.bsp` per the checked-in
//! [`ephemeris::EphemerisManifest`], then run `cargo test -p ephemeris`.
//! Without the variable the interpolation test reports a skip, so a plain
//! `cargo test` stays green everywhere the kernels are absent.

use ephemeris::{load, EphemerisManifest, FilesystemStore, TdbSeconds, DATA_DIR_ENV};
use serde::Deserialize;

/// The checked-in provenance manifest, parsed here so structural mistakes
/// (bad digest format, duplicate names, missing files) fail the test suite.
const MANIFEST_JSON: &str = include_str!("../manifest.json");

/// The checked-in Horizons reference table.
const REFERENCE_JSON: &str = include_str!("../fixtures/horizons_reference.json");

/// One Horizons reference position, keyed by its TDB Julian date.
#[derive(Deserialize)]
struct ReferencePosition {
    jd_tdb: f64,
    x_km: f64,
    y_km: f64,
    z_km: f64,
}

/// One body's reference positions and the tolerance they must meet.
#[derive(Deserialize)]
struct ReferenceBody {
    name: String,
    naif_id: i32,
    tolerance_km: f64,
    positions: Vec<ReferencePosition>,
}

/// The full table: provenance header plus the per-body positions.
#[derive(Deserialize)]
struct ReferenceTable {
    generated: String,
    note: String,
    bodies: Vec<ReferenceBody>,
}

/// Julian date of the J2000 epoch; TDB seconds past J2000 are measured from
/// this instant.
const J2000_JD: f64 = 2_451_545.0;

#[test]
fn checked_in_manifest_pins_both_kernels() {
    let manifest: EphemerisManifest = serde_json::from_str(MANIFEST_JSON).expect("manifest parses");
    assert_eq!(manifest.version, 1);
    assert_eq!(manifest.files.len(), 2, "DE440 and Chiron are pinned");
    assert_eq!(manifest.files[0].name, "de440.bsp");
    assert_eq!(manifest.files[0].bytes, 119_799_808);
    assert_eq!(
        manifest.files[0].sha256,
        "a4ce9bf9b3282becc9f4b2ac3cebe03a2ae7599981aabd7265fd8482fff7c4b5"
    );
    assert_eq!(manifest.files[1].name, "chiron.bsp");
    assert_eq!(manifest.files[1].bytes, 2_791_424);
    // The time-scale model is reproducibility provenance: it must name the
    // leap-second source, the TAI→TT offset, and the TDB periodic term.
    for term in ["leap-second", "32.184", "periodic"] {
        assert!(
            manifest.time_scale_model.contains(term),
            "time_scale_model must document {term}"
        );
    }
}

#[test]
fn reference_table_covers_every_body_across_the_grid() {
    let table: ReferenceTable = serde_json::from_str(REFERENCE_JSON).expect("table parses");
    // Provenance must be present: when the table was generated and the
    // documented DE441-vs-DE440 agreement basis for its tolerances.
    assert!(!table.generated.is_empty(), "generation date recorded");
    assert!(
        table.note.contains("DE441"),
        "the note must state the Horizons solution source"
    );
    assert_eq!(table.bodies.len(), 13, "12 DE440 bodies plus Chiron");
    for body in &table.bodies {
        assert_eq!(body.positions.len(), 16, "{} grid coverage", body.name);
        assert!(body.tolerance_km > 0.0, "{} tolerance", body.name);
    }
}

#[test]
fn interpolation_matches_the_horizons_reference() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("skipping: {DATA_DIR_ENV} is unset, so the real kernels are not available");
        return;
    }
    let manifest: EphemerisManifest = serde_json::from_str(MANIFEST_JSON).expect("manifest parses");
    let store = FilesystemStore::from_env().expect("data dir present");
    let loaded = load(&store, &manifest).expect("kernels load and verify");

    let table: ReferenceTable = serde_json::from_str(REFERENCE_JSON).expect("table parses");
    for body in &table.bodies {
        for position in &body.positions {
            // JDTDB → TDB seconds past J2000.
            let t = TdbSeconds((position.jd_tdb - J2000_JD) * 86_400.0);
            let computed = loaded
                .ephemeris
                .barycentric_position_id(body.naif_id, t)
                .unwrap_or_else(|error| panic!("{} at JD {}: {error}", body.name, position.jd_tdb));
            let reference = [position.x_km, position.y_km, position.z_km];
            for axis in 0..3 {
                let delta = (computed[axis] - reference[axis]).abs();
                assert!(
                    delta <= body.tolerance_km,
                    "{} axis {} at JD {}: crate {:.6} km vs Horizons {:.6} km, delta {:.6} km > {:.6} km",
                    body.name,
                    axis,
                    position.jd_tdb,
                    computed[axis],
                    reference[axis],
                    delta,
                    body.tolerance_km
                );
            }
        }
    }
}
