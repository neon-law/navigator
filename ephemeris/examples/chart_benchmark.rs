//! Synthetic complete-chart timings; prints durations without input or output values.

use chrono::NaiveDate;
use ephemeris::{
    AstrologyChartProvider, AstrologyChartRequest, BirthTime, EphemerisManifest, FilesystemStore,
    HouseSystem, LocalBirthTime,
};
use std::{hint::black_box, num::NonZeroUsize, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manifest: EphemerisManifest = serde_json::from_str(include_str!("../manifest.json"))?;
    let store = FilesystemStore::from_env()?;
    let started = Instant::now();
    let provider = AstrologyChartProvider::load(
        &store,
        &manifest,
        NonZeroUsize::new(16).ok_or("positive cache capacity")?,
    )?;
    let load = started.elapsed();
    let mut request = AstrologyChartRequest::new(
        BirthTime::Exact(LocalBirthTime {
            local: NaiveDate::from_ymd_opt(2000, 1, 1)
                .and_then(|date| date.and_hms_opt(12, 0, 0))
                .ok_or("valid synthetic instant")?,
            zone: chrono_tz::UTC,
            offset_seconds: None,
        }),
        0.0,
        40.0,
    );
    request.settings.houses = HouseSystem::Equal;
    let started = Instant::now();
    black_box(provider.calculate(&request)?);
    let first = started.elapsed();
    let mut uncached = Vec::new();
    let mut cached = Vec::new();
    for _ in 0..100 {
        provider.clear_cache()?;
        let started = Instant::now();
        black_box(provider.calculate(&request)?);
        uncached.push(started.elapsed());
        let started = Instant::now();
        black_box(provider.calculate(&request)?);
        cached.push(started.elapsed());
    }
    uncached.sort_unstable();
    cached.sort_unstable();
    println!("verified load: {load:?}; first chart: {first:?}");
    println!(
        "100-run medians: calculation {:?}; cache hit {:?}",
        uncached[50], cached[50]
    );
    provider.clear_cache()?;
    Ok(())
}
