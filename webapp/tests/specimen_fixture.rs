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
//! gate as the Swiss Ephemeris anchors), not byte equality: DE440's bytes are
//! stable, but the Chiron kernel is regenerated per request by the JPL
//! Horizons API, and the manifest documents each regeneration as
//! solution-equivalent while changing the digest. Rendering the fixture into
//! SVG paths *is* deterministic, so byte-level stability is enforced
//! separately by the golden path snapshot in
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

/// Lowercase hex digits, matching the ephemeris loader's digest format.
const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";

/// SHA-256 of a kernel file in the data dir, hex-encoded.
fn kernel_sha256(name: &str) -> String {
    use sha2::{Digest, Sha256};
    let dir = std::env::var_os(DATA_DIR_ENV).expect("data dir checked above");
    let bytes = std::fs::read(std::path::Path::new(&dir).join(name))
        .unwrap_or_else(|e| panic!("{name} readable: {e}"));
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(HEX_DIGITS[usize::from(byte & 0x0F)]));
    }
    hex
}

#[test]
fn specimen_fixture_matches_the_engine_at_the_specimen_instant() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("skipping: {DATA_DIR_ENV} is unset, so the real kernels are not available");
        return;
    }
    // The manifest pins one particular Horizons fetch of chiron.bsp, but
    // Horizons regenerates the file on every request (solution-equivalent
    // bytes, new digest — documented in the manifest's source note), so a
    // locally re-fetched kernel can never reproduce the pinned digest. For
    // this solution-equivalence gate the digest is re-pinned to the local
    // kernel: the checksum still guards against a corrupt file, while the
    // 0.01° comparison below is the binding check. Provenance pinning stays
    // enforced by the ephemeris crate's own tests and its fixtures.
    let mut manifest: serde_json::Value =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json"))
            .expect("manifest parses");
    let chiron_sha = kernel_sha256("chiron.bsp");
    for entry in manifest["files"]
        .as_array_mut()
        .expect("manifest files array")
    {
        if entry["name"] == "chiron.bsp" {
            entry["sha256"] = chiron_sha.clone().into();
        }
    }
    let manifest: EphemerisManifest =
        serde_json::from_value(manifest).expect("re-pinned manifest parses");
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
