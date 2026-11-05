//! The kernel-gated specimen-fixture gate: re-cast the specimen instant
//! through the real line engine and compare against the embedded fixture.
//!
//! [`webapp::astrocartography::specimen_lines`] serves the snapshot shipped
//! in `webapp/src/assets/world-specimen-lines.json`. This test proves the
//! snapshot still matches the engine that produced it. Skipped — like the
//! ephemeris crate's own kernel-gated anchors — when
//! [`ephemeris::DATA_DIR_ENV`] is unset, because the 119 MB of kernels stay
//! outside the repository.
//!
//! The comparison runs at the project's binding line gate (0.01°, the same
//! gate as the Swiss Ephemeris anchors). Both kernel files must match the
//! exact manifest pins; regenerated Horizons files require separate validation
//! before their pins can change. Rendering the fixture into SVG paths is
//! deterministic and covered by the golden path snapshot in
//! `webapp/src/astrocartography.rs`.

use ephemeris::lines::{lines_for, Angle};
use ephemeris::{load, EphemerisManifest, FilesystemStore, DATA_DIR_ENV};
use hifitime::Epoch;

/// The spec's binding line gate: 0.01°, about 1 km on the ground.
const GATE_DEG: f64 = 0.01;

/// The specimen instant, matching `webapp::astrocartography::SPECIMEN_INSTANT`.
fn specimen_epoch() -> Epoch {
    Epoch::maybe_from_gregorian_utc(2000, 1, 1, 12, 0, 0, 0).expect("valid specimen instant")
}

#[test]
fn specimen_fixture_matches_the_engine_at_the_specimen_instant() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("skipping: {DATA_DIR_ENV} is unset, so the real kernels are not available");
        return;
    }
    let manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json"))
            .expect("manifest parses");
    let store = FilesystemStore::from_env().expect("data dir present");
    let loaded = load(&store, &manifest).expect("kernels load and verify");

    let computed = lines_for(&loaded, specimen_epoch()).expect("line engine computes");
    let embedded = webapp::astrocartography::specimen_lines().expect("embedded fixture parses");

    assert_eq!(computed.len(), embedded.len(), "line count");
    for (computed_line, embedded_line) in computed.iter().zip(&embedded) {
        assert_eq!(computed_line.body, embedded_line.body, "body order");
        assert_eq!(computed_line.angle, embedded_line.angle, "angle order");
        assert_eq!(
            computed_line.points.len(),
            embedded_line.points.len(),
            "point count for {} {:?}",
            computed_line.body.name(),
            computed_line.angle
        );
        for (computed_point, embedded_point) in
            computed_line.points.iter().zip(&embedded_line.points)
        {
            let drift = (computed_point.0 - embedded_point.0)
                .abs()
                .max((computed_point.1 - embedded_point.1).abs());
            assert!(
                drift < GATE_DEG,
                "drift {}° for {} {:?} exceeds the {GATE_DEG}° gate",
                drift,
                computed_line.body.name(),
                computed_line.angle
            );
        }
    }
}

/// Guards the gate itself: the four angles per body stay in generation order,
/// so a future engine change that reorders output fails loudly instead of
/// silently passing the pointwise comparison against a shuffled fixture.
#[test]
fn fixture_rows_carry_all_four_angles_per_body() {
    let embedded = webapp::astrocartography::specimen_lines().expect("embedded fixture parses");
    for chunk in embedded.chunks(4) {
        assert_eq!(
            chunk.len(),
            4,
            "incomplete angle group for {:?}",
            chunk[0].body
        );
        let angles: Vec<Angle> = chunk.iter().map(|line| line.angle).collect();
        assert_eq!(
            angles,
            vec![Angle::Mc, Angle::Ic, Angle::Asc, Angle::Dsc],
            "angle order for {:?}",
            chunk[0].body
        );
    }
}
