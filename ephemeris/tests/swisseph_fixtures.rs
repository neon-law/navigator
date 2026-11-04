//! Swiss Ephemeris fixture-table validation for the apparent-position layer.
//!
//! The reference table ([`FIXTURE_JSON`]) holds apparent geocentric positions
//! from Swiss Ephemeris 2.10.03 (`swe_calc`, `SEFLG_SWIEPH`): ecliptic
//! longitude/latitude on the true equinox of date and RA/Dec on the true
//! equator of date, for the Sun through Pluto, Chiron, and the mean lunar
//! node. The JSON's own header carries the full provenance. Body rows cover a
//! decadal grid from 1900 to 2050 plus the solstice/lunar-extremity epochs;
//! mean-node rows cover a ~92-day grid across the same span (the node moves
//! ~1.6° per month, so a denser grid is what actually exercises the mean
//! elements).
//!
//! The comparison gate is the spec's 1 arcsecond on every angle. The body
//! rows need the real kernel files (see [`ephemeris::DATA_DIR_ENV`]) and are
//! skipped when they are absent; the mean-node rows are pure arithmetic and
//! always run.
//!
//! Swiss Ephemeris's nutation model differs from the crate's IAU 2000B port
//! by up to ~0.2″ over this span, and its obliquity polynomial differs from
//! IAU 2006 by ~0.03″; both sit inside the gate with margin. The fixture
//! Julian dates are TT (`swe_calc` ephemeris time) while the crate works in
//! TDB; the ≤ 1.7 ms difference moves positions by ≤ 0.001″ at lunar
//! distance.

use ephemeris::{
    apparent_geocentric, load, mean_node, Body, EphemerisManifest, FilesystemStore, TdbSeconds,
    DATA_DIR_ENV,
};
use serde::Deserialize;

/// The checked-in Swiss Ephemeris reference table.
const FIXTURE_JSON: &str = include_str!("../fixtures/swisseph_apparent.json");

/// One Swiss Ephemeris reference position.
#[derive(Deserialize)]
struct SwissephRow {
    jd_tdb: f64,
    body: String,
    lon: f64,
    lat: f64,
    ra: f64,
    dec: f64,
}

/// The full table: provenance header plus the position rows.
#[derive(Deserialize)]
struct SwissephTable {
    generator: String,
    swisseph_version: String,
    flags: String,
    rows: Vec<SwissephRow>,
}

/// Julian date of the J2000 epoch; TDB seconds past J2000 are measured from
/// this instant.
const J2000_JD: f64 = 2_451_545.0;

/// The spec's accuracy gate: 1 arcsecond, in degrees.
const GATE_DEG: f64 = 1.0 / 3600.0;

/// The crate `Body` for a fixture body name; `None` for the mean node, which
/// is not a body.
fn body_of(name: &str) -> Option<Body> {
    match name {
        "sun" => Some(Body::Sun),
        "moon" => Some(Body::Moon),
        "mercury" => Some(Body::Mercury),
        "venus" => Some(Body::Venus),
        "mars" => Some(Body::Mars),
        "jupiter" => Some(Body::Jupiter),
        "saturn" => Some(Body::Saturn),
        "uranus" => Some(Body::Uranus),
        "neptune" => Some(Body::Neptune),
        "pluto" => Some(Body::Pluto),
        "chiron" => Some(Body::Chiron),
        _ => None,
    }
}

/// The signed angular difference `a − b` in degrees, wrapped to \[−180, 180].
fn angle_difference_degrees(a: f64, b: f64) -> f64 {
    (a - b + 180.0).rem_euclid(360.0) - 180.0
}

/// Asserts every angle stays inside the gate, with a message that carries the
/// failing delta in arcseconds.
fn assert_within_gate(label: &str, body: &str, jd: f64, computed: f64, reference: f64) {
    let delta = angle_difference_degrees(computed, reference);
    assert!(
        delta.abs() <= GATE_DEG,
        "{label} for {body} at JD {jd}: crate {computed:.9}° vs swisseph {reference:.9}°, \
         delta {} arcsec > 1 arcsec",
        delta * 3600.0
    );
}

/// `TdbSeconds` for a fixture Julian date.
fn seconds(jd_tdb: f64) -> TdbSeconds {
    TdbSeconds((jd_tdb - J2000_JD) * 86_400.0)
}

#[test]
fn fixture_provenance_records_the_generator() {
    let table: SwissephTable = serde_json::from_str(FIXTURE_JSON).expect("table parses");
    assert!(
        table.generator.contains("swisseph"),
        "generator provenance recorded: {}",
        table.generator
    );
    assert_eq!(
        table.swisseph_version, "2.10.03",
        "swisseph version recorded"
    );
    assert!(
        table.flags.contains("SEFLG_SWIEPH"),
        "flags name the ephemeris flag basis: {}",
        table.flags
    );
    let node_rows = table
        .rows
        .iter()
        .filter(|row| row.body == "mean_node")
        .count();
    let body_rows = table
        .rows
        .iter()
        .filter(|row| body_of(&row.body).is_some())
        .count();
    assert_eq!(body_rows, 451, "11 bodies across 41 epochs");
    assert!(node_rows >= 601, "dense node grid, got {node_rows}");
    for row in &table.rows {
        assert!(
            (1900.0..=2051.0).contains(&swisseph_year(row.jd_tdb)),
            "row {} JD {} inside the 1900-2050 grid",
            row.body,
            row.jd_tdb
        );
    }
}

/// Calendar year of a Julian date, good enough for grid-range assertions.
fn swisseph_year(jd: f64) -> f64 {
    2000.0 + (jd - J2000_JD) / 365.25
}

/// The mean lunar node rows are pure arithmetic — no kernels — so this test
/// always runs. Every angle must sit inside the 1″ gate.
#[test]
fn mean_node_matches_the_swisseph_grid() {
    let table: SwissephTable = serde_json::from_str(FIXTURE_JSON).expect("table parses");
    let mut checked = 0;
    for row in &table.rows {
        if row.body != "mean_node" {
            continue;
        }
        checked += 1;
        let computed = mean_node(seconds(row.jd_tdb));
        assert_within_gate(
            "node longitude",
            "mean_node",
            row.jd_tdb,
            computed.lon_deg,
            row.lon,
        );
        assert_within_gate("node ra", "mean_node", row.jd_tdb, computed.ra_deg, row.ra);
        assert_within_gate(
            "node dec",
            "mean_node",
            row.jd_tdb,
            computed.dec_deg,
            row.dec,
        );
        assert!(
            computed.lat_deg.abs() < 1e-9,
            "node latitude must be exactly on the ecliptic, got {}",
            computed.lat_deg
        );
    }
    assert!(checked >= 601, "checked {checked} node rows");
}

/// The physical-body rows walk the full chain: SPK positions, light time,
/// aberration, and the frame rotation. They need the real kernels.
#[test]
fn body_rows_match_the_swisseph_grid() {
    if std::env::var_os(DATA_DIR_ENV).is_none() {
        eprintln!("skipping: {DATA_DIR_ENV} is unset, so the real kernels are not available");
        return;
    }
    let manifest: EphemerisManifest =
        serde_json::from_str(include_str!("../manifest.json")).expect("manifest parses");
    let store = FilesystemStore::from_env().expect("data dir present");
    let loaded = load(&store, &manifest).expect("kernels load and verify");

    let table: SwissephTable = serde_json::from_str(FIXTURE_JSON).expect("table parses");
    let mut checked = 0;
    for row in &table.rows {
        let Some(body) = body_of(&row.body) else {
            continue;
        };
        checked += 1;
        let computed = apparent_geocentric(&loaded.ephemeris, body, seconds(row.jd_tdb))
            .unwrap_or_else(|error| panic!("{} at JD {}: {error}", row.body, row.jd_tdb));
        assert_within_gate(
            "longitude",
            &row.body,
            row.jd_tdb,
            computed.lon_deg,
            row.lon,
        );
        assert_within_gate("latitude", &row.body, row.jd_tdb, computed.lat_deg, row.lat);
        assert_within_gate("ra", &row.body, row.jd_tdb, computed.ra_deg, row.ra);
        assert_within_gate("dec", &row.body, row.jd_tdb, computed.dec_deg, row.dec);
    }
    assert_eq!(checked, 451, "every body row checked");
}
