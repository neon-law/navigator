//! Regenerate only the fixed public chart reference; accepts no personal input.

use ephemeris::lines::LineBody;
use ephemeris::{AstrologyChartProvider, AstrologyChartRequest, BirthTime, LocalBirthTime};
use sha2::Digest;
use std::fmt::Write as _;
use std::num::NonZeroUsize;

fn main() {
    let mut manifest: ephemeris::EphemerisManifest =
        serde_json::from_str(include_str!("../../ephemeris/manifest.json"))
            .expect("checked-in source manifest");
    manifest.files.retain(|file| file.name == "de440.bsp");
    let provider = AstrologyChartProvider::load(
        &ephemeris::FilesystemStore::from_env().expect("exact reference kernel directory"),
        &manifest,
        NonZeroUsize::new(1).expect("positive reference cache capacity"),
    )
    .expect("exact source pins must load without replacement");
    let instant = webapp::astrocartography::SPECIMEN_INSTANT
        .parse::<chrono::DateTime<chrono::Utc>>()
        .expect("fixed public reference clock");
    let mut request = AstrologyChartRequest::new(
        BirthTime::Exact(LocalBirthTime {
            local: instant.naive_utc(),
            zone: "UTC".parse().expect("reference zone"),
            offset_seconds: None,
        }),
        0.0,
        0.0,
    );
    request.settings.houses = ephemeris::HouseSystem::Equal;
    // Explicit public reference inventory; future minor-body additions are not inherited.
    request.bodies = vec![
        LineBody::Sun,
        LineBody::Moon,
        LineBody::Mercury,
        LineBody::Venus,
        LineBody::Mars,
        LineBody::Jupiter,
        LineBody::Saturn,
        LineBody::Uranus,
        LineBody::Neptune,
        LineBody::Pluto,
        LineBody::MeanNode,
    ];
    let response = provider
        .calculate(&request)
        .expect("fixed reference chart calculates");
    let chart = webapp::astrology_chart::calculated::CalculatedChart::from_response(&response)
        .expect("validated reference adapts");
    let mut document = serde_json::to_value(&chart).expect("finite reference serializes");
    let mut generator_sha256 = String::with_capacity(64);
    for byte in sha2::Sha256::digest(include_bytes!("chart_specimen.rs")) {
        write!(generator_sha256, "{byte:02x}").expect("formatting into a String");
    }
    document["metadata"]["reference"] = serde_json::json!({
        "generator": "webapp/examples/chart_specimen.rs",
        "generator_sha256": generator_sha256,
        "instant_utc": webapp::astrocartography::SPECIMEN_INSTANT,
        "longitude_deg": 0.0,
        "latitude_deg": 0.0,
        "inventory": "DE440 core bodies and mean node; no minor bodies",
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&document).expect("reference metadata serializes")
    );
}
