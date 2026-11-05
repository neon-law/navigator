//! Bounded, private in-memory chart calculations from verified kernel bytes.

use crate::lines::LineBody;
use crate::{
    AstrologyChartRequest, BirthTime, ChartAngles, ChartError, EphemerisManifest, EphemerisStore,
    LoadedEphemeris, ResolvedBirthTime, TdbSeconds,
};
use chrono::{Datelike, Timelike};
use hifitime::Epoch;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    num::NonZeroUsize,
    sync::{Arc, Mutex},
};

const DAY_SECONDS: f64 = 86_400.0;
const RATE_STEP_SECONDS: f64 = 30.0;
const ALGORITHM: &str =
    "navigator-local-SPK/apparent-IAU2006-2000B/IERS2003-node/USNO-deltaT/chart-v2";

/// One body's apparent geocentric state. Orbital points have no physical range.
#[derive(Clone, Serialize)]
pub struct BodyPosition {
    /// Stable body key, separate from a display label or fixed-star catalog.
    pub body: &'static str,
    /// Target ID in the source SPK, absent for the mean lunar node.
    pub source_id: Option<i32>,
    /// Tropical ecliptic longitude of date, [0, 360), degrees.
    pub longitude_deg: f64,
    /// Ecliptic latitude of date, degrees.
    pub latitude_deg: f64,
    /// Right ascension on the true equator of date, degrees.
    pub right_ascension_deg: f64,
    /// Declination on the true equator of date, degrees.
    pub declination_deg: f64,
    /// Light-time-corrected distance, kilometers; absent for orbital points.
    pub distance_km: Option<f64>,
    /// Signed longitude speed, degrees per day, using a 60-second central window.
    pub longitude_speed_deg_per_day: f64,
    /// Signed latitude speed, degrees per day.
    pub latitude_speed_deg_per_day: f64,
    /// Signed range speed, kilometers per day; absent for orbital points.
    pub distance_speed_km_per_day: Option<f64>,
}

/// Distinct time-scale readings for the evaluation instant.
#[derive(Clone, Serialize)]
pub struct ChartTimeScales {
    /// Civil UTC Julian day.
    pub julian_day_utc: f64,
    /// Terrestrial Time Julian day.
    pub julian_day_tt: f64,
    /// Barycentric Dynamical Time Julian day used by SPK interpolation.
    pub julian_day_tdb: f64,
    /// Earth-rotation model's UT1 Julian day, calculated as TT minus delta-T.
    pub model_julian_day_ut1: f64,
    /// Model TT minus UT1, seconds, evaluated at UTC.
    pub delta_t_seconds: f64,
    /// Observation, forecast, or explicitly unbounded extrapolation.
    pub delta_t_kind: crate::lines::deltat::DeltaTKind,
    /// Quoted source error, not a guaranteed model/interpolation bound.
    pub delta_t_reported_error_seconds: Option<f64>,
}

/// Source bytes and actual numerical conventions behind a private chart result.
#[derive(Clone, Serialize)]
pub struct ChartProvenance {
    /// Numerical algorithm identity.
    pub algorithm: &'static str,
    /// Navigator release version.
    pub version: &'static str,
    /// Exact loaded kernel identities and time/frame conventions.
    pub manifest: EphemerisManifest,
    /// IANA timezone rules version.
    pub tzdb_version: &'static str,
    /// Delta-T model used for Earth rotation.
    pub delta_t_model: &'static str,
    /// Verified Earth-rotation data snapshots, separate from the SPK sources.
    pub delta_t_sources: &'static [crate::lines::deltat::DeltaTDataFile],
    /// Apparent sidereal model used for local angles.
    pub sidereal_model: &'static str,
    /// Mean-node model.
    pub node_model: &'static str,
    /// Time-scale readings, absent when no clock was supplied.
    pub time_scales: Option<ChartTimeScales>,
}

/// A result retains its own original input and never invents unknown-time geometry.
#[derive(Clone)]
pub struct AstrologyChartResponse {
    /// Private original input, including all chosen settings and coordinates.
    pub request: AstrologyChartRequest,
    /// Resolved time and its original precision.
    pub time: ResolvedBirthTime,
    /// Requested positions; absent for an unknown clock.
    pub positions: Option<Vec<BodyPosition>>,
    /// Exact-time angles and optional houses; absent for uncertain or degenerate geometry.
    pub angles: Option<ChartAngles>,
    /// Actual calculation models and source pins.
    pub provenance: ChartProvenance,
}

struct CachedChart {
    key: [u8; 32],
    value: Arc<AstrologyChartResponse>,
}

/// An immutable loaded source and a bounded, clearable, memory-only result cache.
/// The owner controls the provider's lifetime and authorized sharing scope.
pub struct AstrologyChartProvider {
    loaded: LoadedEphemeris,
    capacity: NonZeroUsize,
    cache: Mutex<VecDeque<CachedChart>>,
}

impl AstrologyChartProvider {
    /// Verify and load source bytes once. Request calculations perform no source I/O.
    pub fn load(
        store: &dyn EphemerisStore,
        manifest: &EphemerisManifest,
        capacity: NonZeroUsize,
    ) -> Result<Self, crate::EphemerisError> {
        Ok(Self {
            loaded: crate::load(store, manifest)?,
            capacity,
            cache: Mutex::new(VecDeque::new()),
        })
    }

    /// Calculate only supported conventions, preserving the request's precision.
    /// Errors contain no original birth input or calculated position values.
    pub fn calculate(
        &self,
        request: &AstrologyChartRequest,
    ) -> Result<Arc<AstrologyChartResponse>, ChartError> {
        let time = request.resolve()?;
        let key = self.cache_key(request, &time)?;
        {
            let cache = self
                .cache
                .lock()
                .map_err(|_| ChartError::CacheUnavailable)?;
            if let Some(entry) = cache.iter().find(|entry| entry.key == key) {
                return Ok(Arc::clone(&entry.value));
            }
        }
        let epoch = time.utc().map(epoch_from_utc).transpose()?;
        let positions = epoch
            .map(|epoch| {
                let instant = crate::utc_to_tdb(epoch);
                request
                    .bodies
                    .iter()
                    .map(|body| self.body_position(*body, instant))
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let angles = if time.can_compute_angles() {
            epoch
                .map(|epoch| {
                    let sidereal =
                        crate::lines::sidereal::gast_degrees(epoch) + request.longitude_deg;
                    let obliquity =
                        crate::apparent::frame::true_obliquity(crate::utc_to_tdb(epoch))
                            .to_degrees();
                    crate::chart_angles::chart_angles(
                        sidereal,
                        request.latitude_deg,
                        obliquity,
                        &request.settings.houses,
                    )
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        let provenance = ChartProvenance {
            algorithm: ALGORITHM,
            version: env!("CARGO_PKG_VERSION"),
            manifest: self.loaded.manifest.clone(),
            tzdb_version: time.tzdb_version(),
            delta_t_model: crate::lines::geojson::DELTA_T_MODEL,
            delta_t_sources: crate::lines::deltat::delta_t_sources(),
            sidereal_model: crate::lines::geojson::SIDEREAL_MODEL,
            node_model: crate::lines::geojson::NODE_CONVENTION,
            time_scales: epoch.map(time_scales).transpose()?,
        };
        let result = Arc::new(AstrologyChartResponse {
            request: request.clone(),
            time,
            positions,
            angles,
            provenance,
        });
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| ChartError::CacheUnavailable)?;
        if cache.len() >= self.capacity.get() {
            cache.pop_front();
        }
        cache.push_back(CachedChart {
            key,
            value: Arc::clone(&result),
        });
        Ok(result)
    }

    /// Release all retained private calculation results.
    pub fn clear_cache(&self) -> Result<(), ChartError> {
        self.cache
            .lock()
            .map_err(|_| ChartError::CacheUnavailable)?
            .clear();
        Ok(())
    }

    fn cache_key(
        &self,
        request: &AstrologyChartRequest,
        time: &ResolvedBirthTime,
    ) -> Result<[u8; 32], ChartError> {
        #[derive(Serialize)]
        struct Identity<'a> {
            algorithm: &'static str,
            version: &'static str,
            delta_t_model: &'static str,
            delta_t_sources: &'static [crate::lines::deltat::DeltaTDataFile],
            sidereal_model: &'static str,
            node_model: &'static str,
            manifest: &'a EphemerisManifest,
            tzdb: &'static str,
            original: BirthIdentity,
            utc: Option<String>,
            longitude_bits: u64,
            latitude_bits: u64,
            bodies: Vec<&'static str>,
            settings: &'a crate::ChartSettings,
        }
        let identity = Identity {
            algorithm: ALGORITHM,
            version: env!("CARGO_PKG_VERSION"),
            delta_t_model: crate::lines::geojson::DELTA_T_MODEL,
            delta_t_sources: crate::lines::deltat::delta_t_sources(),
            sidereal_model: crate::lines::geojson::SIDEREAL_MODEL,
            node_model: crate::lines::geojson::NODE_CONVENTION,
            manifest: &self.loaded.manifest,
            tzdb: time.tzdb_version(),
            original: BirthIdentity::new(&request.birth_time),
            utc: time.utc().map(|instant| instant.to_rfc3339()),
            longitude_bits: request.longitude_deg.to_bits(),
            latitude_bits: request.latitude_deg.to_bits(),
            bodies: request.bodies.iter().map(|body| body.name()).collect(),
            settings: &request.settings,
        };
        let bytes = serde_json::to_vec(&identity).map_err(|_| ChartError::SerializationFailed)?;
        let mut key = [0_u8; 32];
        key.copy_from_slice(&Sha256::digest(bytes));
        Ok(key)
    }

    fn body_position(
        &self,
        body: LineBody,
        instant: TdbSeconds,
    ) -> Result<BodyPosition, ChartError> {
        let center = self.sample(body, instant)?;
        let before = self.sample(body, TdbSeconds(instant.0 - RATE_STEP_SECONDS))?;
        let after = self.sample(body, TdbSeconds(instant.0 + RATE_STEP_SECONDS))?;
        let rate = DAY_SECONDS / (2.0 * RATE_STEP_SECONDS);
        let result = BodyPosition {
            body: body.name(),
            source_id: body.spk_body().map(crate::Body::naif_id),
            longitude_deg: center.position.lon_deg,
            latitude_deg: center.position.lat_deg,
            right_ascension_deg: center.position.ra_deg,
            declination_deg: center.position.dec_deg,
            distance_km: center.distance_km,
            longitude_speed_deg_per_day: ((after.position.lon_deg - before.position.lon_deg
                + 180.0)
                .rem_euclid(360.0)
                - 180.0)
                * rate,
            latitude_speed_deg_per_day: (after.position.lat_deg - before.position.lat_deg) * rate,
            distance_speed_km_per_day: before
                .distance_km
                .zip(after.distance_km)
                .map(|(before, after)| (after - before) * rate),
        };
        if [
            result.longitude_deg,
            result.latitude_deg,
            result.right_ascension_deg,
            result.declination_deg,
            result.longitude_speed_deg_per_day,
            result.latitude_speed_deg_per_day,
        ]
        .into_iter()
        .chain(result.distance_km)
        .chain(result.distance_speed_km_per_day)
        .any(|value| !value.is_finite())
        {
            return Err(ChartError::CalculationFailed);
        }
        Ok(result)
    }

    fn sample(&self, body: LineBody, instant: TdbSeconds) -> Result<BodySample, ChartError> {
        if let Some(body) = body.spk_body() {
            let state = crate::apparent_geocentric_state(&self.loaded.ephemeris, body, instant)
                .map_err(|_| ChartError::CalculationFailed)?;
            Ok(BodySample {
                position: state.position,
                distance_km: Some(state.distance_km),
            })
        } else {
            Ok(BodySample {
                position: crate::mean_node(instant),
                distance_km: None,
            })
        }
    }
}

struct BodySample {
    position: crate::ApparentPosition,
    distance_km: Option<f64>,
}

#[derive(Serialize)]
struct BirthIdentity {
    kind: &'static str,
    civil: String,
    zone: String,
    offset: Option<i32>,
    uncertainty: Option<u32>,
}
impl BirthIdentity {
    fn new(input: &BirthTime) -> Self {
        match input {
            BirthTime::Exact(local) => Self {
                kind: "exact",
                civil: local.local.to_string(),
                zone: local.zone.to_string(),
                offset: local.offset_seconds,
                uncertainty: None,
            },
            BirthTime::Approximate {
                local,
                uncertainty_seconds,
            } => Self {
                kind: "approximate",
                civil: local.local.to_string(),
                zone: local.zone.to_string(),
                offset: local.offset_seconds,
                uncertainty: Some(uncertainty_seconds.get()),
            },
            BirthTime::Unknown { date, zone } => Self {
                kind: "unknown",
                civil: date.to_string(),
                zone: zone.to_string(),
                offset: None,
                uncertainty: None,
            },
        }
    }
}

fn epoch_from_utc(utc: chrono::DateTime<chrono::Utc>) -> Result<Epoch, ChartError> {
    let component = |value| u8::try_from(value).map_err(|_| ChartError::CalculationFailed);
    Epoch::maybe_from_gregorian_utc(
        utc.year(),
        component(utc.month())?,
        component(utc.day())?,
        component(utc.hour())?,
        component(utc.minute())?,
        component(utc.second())?,
        utc.nanosecond(),
    )
    .map_err(|_| ChartError::CalculationFailed)
}

fn time_scales(epoch: Epoch) -> Result<ChartTimeScales, ChartError> {
    let julian_day_utc = epoch.to_jde_utc_days();
    let julian_day_tdb = 2_451_545.0 + crate::utc_to_tdb(epoch).0 / DAY_SECONDS;
    let estimate = crate::lines::deltat::delta_t_estimate(julian_day_utc)
        .ok_or(ChartError::CalculationFailed)?;
    let julian_day_tt = epoch.to_jde_tt_days();
    Ok(ChartTimeScales {
        julian_day_utc,
        julian_day_tt,
        julian_day_tdb,
        model_julian_day_ut1: julian_day_tt - estimate.seconds / DAY_SECONDS,
        delta_t_seconds: estimate.seconds,
        delta_t_kind: estimate.kind,
        delta_t_reported_error_seconds: estimate.reported_error_seconds,
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn modeled_ut1_is_derived_from_tt_instead_of_tdb() {
        let epoch = Epoch::maybe_from_gregorian_utc(2000, 4, 1, 0, 0, 0, 0)
            .expect("valid synthetic time-scale reference");
        let scales = time_scales(epoch).expect("valid synthetic time scales");
        assert!(
            (scales.model_julian_day_ut1
                - (scales.julian_day_tt - scales.delta_t_seconds / DAY_SECONDS))
                .abs()
                < 1e-12,
            "Earth rotation must use TT minus delta-T rather than substituting TDB"
        );
    }

    use super::*;
    use crate::lines::LINE_BODIES;
    use crate::spk::test_support::synthetic_type2;
    use crate::{BirthTime, EphemerisFile, EphemerisManifest, EphemerisStore, LocalBirthTime};
    use chrono::NaiveDate;
    use sha2::{Digest, Sha256};
    use std::{
        collections::HashMap,
        num::{NonZeroU32, NonZeroUsize},
        sync::Arc,
    };

    struct MemoryStore(HashMap<String, Vec<u8>>);
    impl EphemerisStore for MemoryStore {
        fn read(&self, name: &str) -> Result<Vec<u8>, crate::EphemerisError> {
            self.0
                .get(name)
                .cloned()
                .ok_or_else(|| crate::EphemerisError::Io {
                    path: name.to_owned(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "synthetic missing kernel",
                    ),
                })
        }
    }

    fn provider(capacity: usize) -> AstrologyChartProvider {
        let radius = 6_000_000_000.0;
        let mut data = HashMap::new();
        let mut files = Vec::new();
        let mut bodies = vec![crate::Body::Earth];
        bodies.extend(LINE_BODIES.iter().filter_map(|body| body.spk_body()));
        let obliquity = 23.439_291_1_f64.to_radians();
        for (index, body) in bodies.iter().enumerate() {
            let index = u32::try_from(index).expect("small synthetic body inventory");
            let zero = [0.0, 0.0];
            let x = if *body == crate::Body::Earth {
                zero
            } else {
                [150_000_000.0 + f64::from(index) * 1_000_000.0, 0.0]
            };
            let (x, y, z) = if *body == crate::Body::Mercury {
                (
                    [150_000_000.0, 0.0],
                    [0.0, -20.0 * obliquity.cos() * radius],
                    [0.0, -20.0 * obliquity.sin() * radius],
                )
            } else {
                (x, zero, zero)
            };
            let kernel = synthetic_type2(
                body.naif_id(),
                0,
                -radius,
                2.0 * radius,
                &[(0.0, radius, &x, &y, &z)],
            );
            let bytes = kernel.raw_bytes_for_test();
            let name = format!("body-{}.bsp", body.naif_id());
            files.push(EphemerisFile {
                upstream_version: "synthetic fixture".to_owned(),
                retrieved_at: chrono::DateTime::from_timestamp(0, 0)
                    .expect("synthetic source timestamp"),
                name: name.clone(),
                sha256: crate::loader::hex_encode(&Sha256::digest(&bytes)),
                bytes: u64::try_from(bytes.len()).expect("small kernel"),
                source: "synthetic://fixture".to_owned(),
                coverage_start: "synthetic broad coverage".to_owned(),
                coverage_end: "synthetic broad coverage".to_owned(),
            });
            data.insert(name, bytes);
        }
        let manifest = EphemerisManifest {
            version: crate::MANIFEST_VERSION,
            time_scale_model: "hifitime UTC/TAI/TT/TDB".to_owned(),
            reference_frame: "ICRF/J2000 frame 1".to_owned(),
            files,
        };
        AstrologyChartProvider::load(
            &MemoryStore(data),
            &manifest,
            NonZeroUsize::new(capacity).expect("positive capacity"),
        )
        .expect("verified synthetic provider")
    }

    fn request() -> crate::AstrologyChartRequest {
        crate::AstrologyChartRequest::new(
            BirthTime::Exact(LocalBirthTime {
                local: NaiveDate::from_ymd_opt(2000, 1, 1)
                    .and_then(|date| date.and_hms_milli_opt(11, 58, 55, 816))
                    .expect("synthetic J2000 clock"),
                zone: chrono_tz::UTC,
                offset_seconds: None,
            }),
            0.0,
            0.0,
        )
    }

    #[test]
    fn unknown_and_approximate_times_disable_actual_returned_geometry() {
        let provider = provider(2);
        let mut input = request();
        input.settings.houses = crate::HouseSystem::Equal;
        input.birth_time = BirthTime::Unknown {
            date: NaiveDate::from_ymd_opt(2000, 1, 1).expect("synthetic date"),
            zone: chrono_tz::UTC,
        };
        let unknown = provider.calculate(&input).expect("valid unknown time");
        assert!(
            unknown.positions.is_none() && unknown.angles.is_none(),
            "unknown time must not fabricate a chart instant"
        );
        let BirthTime::Exact(local) = request().birth_time else {
            panic!("exact synthetic input")
        };
        input.birth_time = BirthTime::Approximate {
            local,
            uncertainty_seconds: NonZeroU32::new(600).expect("uncertainty"),
        };
        let approximate = provider.calculate(&input).expect("valid approximate time");
        assert!(
            approximate.positions.is_some() && approximate.angles.is_none(),
            "approximate positions must not include precision-dependent angles or houses"
        );
    }

    #[test]
    fn physical_states_have_distance_and_signed_speed_across_longitude_wrap() {
        let chart = provider(2).calculate(&request()).expect("synthetic chart");
        let positions = chart.positions.as_ref().expect("known-time positions");
        let mercury = positions
            .iter()
            .find(|row| row.body == "mercury")
            .expect("Mercury requested");
        let expected_speed = -(20.0 / 150_000_000.0_f64).to_degrees() * 86_400.0;
        assert!(
            (mercury.longitude_speed_deg_per_day - expected_speed).abs() < 1e-3,
            "retrograde angular speed must retain its sign through longitude wrap"
        );
        assert!(
            mercury
                .distance_km
                .is_some_and(|distance| (distance - 150_000_000.0).abs() < 1.0),
            "physical distance must be retained in kilometers"
        );
        let node = positions
            .iter()
            .find(|row| row.body == "mean_node")
            .expect("mean node requested");
        assert!(
            node.distance_km.is_none() && node.distance_speed_km_per_day.is_none(),
            "an orbital point must not acquire a physical distance"
        );
    }

    #[test]
    fn cache_keys_preserve_location_settings_and_original_offset_choice() {
        let provider = provider(8);
        let a = request();
        let first = provider.calculate(&a).expect("first chart");
        let repeated = provider.calculate(&a).expect("cached chart");
        assert!(
            Arc::ptr_eq(&first, &repeated),
            "identical full-provenance requests should reuse the result"
        );
        let mut b = a.clone();
        b.longitude_deg = 40.0;
        let moved = provider.calculate(&b).expect("different location");
        assert!(
            !Arc::ptr_eq(&first, &moved),
            "locations must not collide in the cache"
        );
        b = a.clone();
        b.settings.houses = crate::HouseSystem::Equal;
        let houses = provider.calculate(&b).expect("requested houses");
        assert!(
            houses
                .angles
                .as_ref()
                .is_some_and(|angles| angles.house_cusps_deg.is_some()),
            "requested houses must not reuse a result without houses"
        );
        b = a.clone();
        if let BirthTime::Exact(ref mut local) = b.birth_time {
            local.offset_seconds = Some(0);
        }
        let explicit = provider.calculate(&b).expect("explicit original offset");
        assert!(
            !Arc::ptr_eq(&first, &explicit),
            "original offset choices must not be erased by normalization"
        );
        assert!(
            matches!(explicit.request.birth_time, BirthTime::Exact(ref local) if local.offset_seconds == Some(0)),
            "the cached response must retain its own original input"
        );
    }

    #[test]
    fn the_cache_is_bounded_and_can_be_cleared() {
        let provider = provider(1);
        let a = request();
        let first = provider.calculate(&a).expect("first");
        let mut b = a.clone();
        b.latitude_deg = 40.0;
        provider.calculate(&b).expect("second");
        let after_eviction = provider.calculate(&a).expect("after eviction");
        assert!(
            !Arc::ptr_eq(&first, &after_eviction),
            "capacity bounds must evict retained results"
        );
        provider.clear_cache().expect("clear ephemeral results");
        let after_clear = provider.calculate(&a).expect("after clear");
        assert!(
            !Arc::ptr_eq(&after_eviction, &after_clear),
            "clearing must release cached chart results"
        );
    }

    #[test]
    fn parallel_requests_do_not_share_another_inputs_geometry_or_echo() {
        let provider = provider(4);
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..12)
                .map(|index| {
                    let provider = &provider;
                    scope.spawn(move || {
                        let mut input = request();
                        input.longitude_deg = f64::from(index) * 10.0;
                        let result = provider.calculate(&input).expect("parallel chart");
                        assert!(
                            (result.request.longitude_deg - input.longitude_deg).abs()
                                < f64::EPSILON,
                            "each result must preserve its own request"
                        );
                    })
                })
                .collect();
            for handle in handles {
                handle.join().expect("isolated synthetic request");
            }
        });
    }

    #[test]
    fn an_unsupported_house_request_is_rejected_before_any_fallback() {
        let provider = provider(1);
        let mut input = request();
        input.latitude_deg = 75.0;
        input.settings.houses = crate::HouseSystem::Placidus;
        assert!(
            matches!(
                provider.calculate(&input),
                Err(crate::ChartError::UnsupportedSetting(_))
            ),
            "unsupported high-latitude houses must never be replaced silently"
        );
    }
}
