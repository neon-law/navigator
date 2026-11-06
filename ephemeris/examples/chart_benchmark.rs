//! Synthetic complete-chart measurements; prints durations and counts, never chart data.

use chrono::NaiveDate;
use ephemeris::{
    AstrologyChartProvider, AstrologyChartRequest, BirthTime, EphemerisManifest, FilesystemStore,
    HouseSystem, LocalBirthTime,
};
use std::{hint::black_box, num::NonZeroUsize, time::Instant};

const SAMPLES: usize = 100;

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
    let request = synthetic_request()?;
    let first = timed(|| provider.calculate_uncached(&request))?;
    let calculations = calculation_samples(
        SAMPLES,
        || provider.calculate_uncached(&request),
        || provider.calculate(&request),
        || provider.clear_cache(),
    )?;
    let response = provider.calculate_uncached(&request)?;
    let geometry = ephemeris::AstrocartographyGeometry::from_response(&response)?
        .ok_or(ephemeris::ChartError::InvalidGeometry)?;
    let geometry_times = repeated_samples(SAMPLES, || {
        ephemeris::AstrocartographyGeometry::from_response(&response)?
            .ok_or(ephemeris::ChartError::InvalidGeometry)
    })?;
    let rows = repeated_samples(SAMPLES, || {
        Ok::<_, ephemeris::ChartError>(geometry.point_rows().collect::<Vec<_>>())
    })?;
    let geojson = repeated_samples(SAMPLES, || geometry.geojson())?;
    println!("verified load: {load:?}; first private chart: {first:?}");
    println!(
        "geometry inventory: lines={}; rows={}",
        geometry.lines().len(),
        geometry.point_rows().count()
    );
    for (label, samples) in [
        ("private calculate_uncached", &calculations.private),
        ("cache miss calculate", &calculations.miss),
        ("prewarmed cache hit calculate", &calculations.hit),
        ("canonical geometry", &geometry_times),
        ("ordered row materialization", &rows),
        ("GeoJSON encoding", &geojson),
    ] {
        let summary = summarize(samples).ok_or("nonempty benchmark samples")?;
        println!(
            "{label}: samples={} nearest-rank p50={:?} p95={:?}",
            summary.samples, summary.p50, summary.p95
        );
    }
    provider.clear_cache()?;
    Ok(())
}

fn synthetic_request() -> Result<AstrologyChartRequest, &'static str> {
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
    Ok(request)
}

struct CalculationSamples {
    private: Vec<std::time::Duration>,
    miss: Vec<std::time::Duration>,
    hit: Vec<std::time::Duration>,
}

fn timed<T, E>(operation: impl FnOnce() -> Result<T, E>) -> Result<std::time::Duration, E> {
    let started = Instant::now();
    let value = black_box(operation()?);
    let elapsed = started.elapsed();
    drop(value);
    Ok(elapsed)
}

fn calculation_samples<T, E>(
    count: usize,
    mut private: impl FnMut() -> Result<T, E>,
    mut cached: impl FnMut() -> Result<T, E>,
    mut clear: impl FnMut() -> Result<(), E>,
) -> Result<CalculationSamples, E> {
    let mut samples = CalculationSamples {
        private: Vec::new(),
        miss: Vec::new(),
        hit: Vec::new(),
    };
    for _ in 0..count {
        samples.private.push(timed(&mut private)?);
        clear()?;
        samples.miss.push(timed(&mut cached)?);
        // The hit is independently prewarmed outside its measured operation.
        black_box(cached()?);
        samples.hit.push(timed(&mut cached)?);
    }
    Ok(samples)
}

#[derive(Debug, PartialEq, Eq)]
struct TimingSummary {
    samples: usize,
    p50: std::time::Duration,
    p95: std::time::Duration,
}

// Nearest rank uses ceil(p * n / 100), one-based. In particular p50 for
// an even-sized sample is the lower middle observation, not an averaged median.
fn summarize(samples: &[std::time::Duration]) -> Option<TimingSummary> {
    if samples.is_empty() {
        return None;
    }
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let rank = |percent: usize| {
        (ordered.len() / 100) * percent + ((ordered.len() % 100) * percent).div_ceil(100)
    };
    Some(TimingSummary {
        samples: ordered.len(),
        p50: ordered[rank(50) - 1],
        p95: ordered[rank(95) - 1],
    })
}

fn repeated_samples<T, E>(
    count: usize,
    mut operation: impl FnMut() -> Result<T, E>,
) -> Result<Vec<std::time::Duration>, E> {
    (0..count).map(|_| timed(&mut operation)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn private_miss_and_prewarmed_hit_execute_distinct_operations() {
        let calls = RefCell::new(Vec::new());
        let samples = calculation_samples(
            2,
            || {
                calls.borrow_mut().push("private");
                Ok::<_, ()>(0)
            },
            || {
                calls.borrow_mut().push("cached");
                Ok::<_, ()>(0)
            },
            || {
                calls.borrow_mut().push("clear");
                Ok::<_, ()>(())
            },
        )
        .unwrap();
        assert_eq!(
            samples.private.len(),
            2,
            "private calculations require their own measured calls"
        );
        assert_eq!(samples.miss.len(), 2);
        assert_eq!(samples.hit.len(), 2);
        assert_eq!(
            *calls.borrow(),
            [
                "private", "clear", "cached", "cached", "cached", "private", "clear", "cached",
                "cached", "cached"
            ],
            "clear and hit prewarming are explicit operations outside each measured call"
        );
    }

    #[test]
    fn nearest_rank_percentiles_cover_empty_single_even_and_unsorted_samples() {
        use std::time::Duration;
        assert!(summarize(&[]).is_none());
        let one = Duration::from_nanos(7);
        assert_eq!(
            summarize(&[one]),
            Some(TimingSummary {
                samples: 1,
                p50: one,
                p95: one
            })
        );
        let values = [4, 1, 3, 2].map(Duration::from_nanos);
        assert_eq!(
            summarize(&values),
            Some(TimingSummary {
                samples: 4,
                p50: Duration::from_nanos(2),
                p95: Duration::from_nanos(4)
            })
        );
        assert_eq!(
            values[0],
            Duration::from_nanos(4),
            "observations are not reordered"
        );
        let hundred: Vec<_> = (1..=100).map(Duration::from_nanos).collect();
        assert_eq!(
            summarize(&hundred),
            Some(TimingSummary {
                samples: 100,
                p50: Duration::from_nanos(50),
                p95: Duration::from_nanos(95)
            })
        );
    }
}
