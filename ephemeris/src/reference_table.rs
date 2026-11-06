//! Bounded daily reference data calculated without personal inputs or result caches.

use crate::lines::LineBody;
use crate::{
    BirthTimeReference, BirthTimeScale, BodyPosition, ChartError, ChartInstant, CoordinateFrame,
    DeltaTDataFile, DeltaTKind, EphemerisManifest, EphemerisStore, NorthNode, ObserverOrigin,
    Zodiac,
};
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use hifitime::{Epoch, TimeScale};
use serde::Serialize;

/// Maximum days per in-memory batch, independent of the full supported date domain.
pub const MAX_REFERENCE_BATCH_DAYS: i64 = 366;
const BODIES: [LineBody; 11] = [
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

/// Fail-closed errors for reference calculations; no personal chart input is accepted.
#[derive(Debug, thiserror::Error)]
pub enum ReferenceTableError {
    /// Empty, reversed, outside-domain or oversized batches are refused before source reads.
    #[error("reference interval must contain 1 to 366 days inside 1900-01-01 through 2049-12-31")]
    InvalidInterval,
    /// The compiled eligible source declaration is unavailable.
    #[error("eligible reference source declaration is unavailable")]
    InvalidSource,
    /// Source bytes did not load under their declared pin.
    #[error(transparent)]
    Source(#[from] crate::EphemerisError),
    /// A supported exact calculation could not be completed.
    #[error(transparent)]
    Calculation(#[from] ChartError),
}

/// Reusable daily celestial rows; this is not a saved chart or an artifact release.
#[derive(Clone, Serialize)]
pub struct EphemerisReferenceTable {
    /// Row contract version, independent of source-manifest and model versions.
    pub schema_version: u32,
    /// First included universal civil day.
    pub from: NaiveDate,
    /// First excluded universal civil day.
    pub to_exclusive: NaiveDate,
    /// Nonpersonal numerical conventions and exact source identities.
    pub metadata: ReferenceMetadata,
    /// Date-major rows, followed by the fixed eligible body order.
    pub rows: Vec<EphemerisReferenceRow>,
}

/// Reference-only model metadata, excluding timezone rules and private chart provenance.
#[derive(Clone, Serialize)]
pub struct ReferenceMetadata {
    /// Shared exact-instant numerical algorithm.
    pub algorithm: &'static str,
    /// Navigator implementation release.
    pub provider_version: &'static str,
    /// Verified raw-source identity; this is not a derived-release manifest.
    pub source_manifest: EphemerisManifest,
    /// Selected delta-T model and its verified public source snapshots.
    pub delta_t_model: &'static str,
    /// Earth-rotation data identities, separate from the orbital kernel.
    pub delta_t_sources: &'static [DeltaTDataFile],
    /// Mean ascending node convention, independent of physical-body corrections.
    pub node_model: &'static str,
    /// Longitude convention.
    pub zodiac: Zodiac,
    /// Observer at the Earth's center.
    pub origin: ObserverOrigin,
    /// True equator and ecliptic of date, separate from the raw SPK frame.
    pub frame: CoordinateFrame,
    /// Explicit precession/nutation convention for the output, distinct from raw ICRF sources.
    pub frame_model: &'static str,
    /// Only the mean ascending node is provided.
    pub north_node: NorthNode,
    /// Applied physical-body direction corrections, separate from the analytic mean node.
    pub spk_corrections: &'static [&'static str],
    /// No gravitational deflection is applied.
    pub gravitational_deflection: bool,
    /// No atmospheric refraction is applied.
    pub atmospheric_refraction: bool,
    /// Angular columns are degrees; signed angular rates use degrees per day.
    pub angular_unit: &'static str,
    /// Distances are kilometers; signed range rates use kilometers per day.
    pub distance_unit: &'static str,
    /// Rate denominator; one day is 86,400 TDB-coordinate seconds for these derivatives.
    pub rate_time_unit: &'static str,
    /// Derivatives use TDB coordinate time, not civil UTC calendar-day intervals.
    pub rate_time_scale: &'static str,
    /// Centered finite-difference window, in TDB-coordinate seconds (plus/minus 30).
    pub rate_window_seconds: f64,
    /// Each coordinate's origin is noon in its own scale, not one common physical instant.
    pub tt_nanoseconds_origin: &'static str,
    /// TDB calendar-coordinate origin.
    pub tdb_nanoseconds_origin: &'static str,
    /// The native SPK argument retains the evaluator's floating-point seconds.
    pub spk_argument: &'static str,
    /// Daily rows do not authorize interpolation at arbitrary instants.
    pub interpolation_supported: bool,
}

/// One exact daily sample. Integer time coordinates preserve the computed clock representation,
/// without claiming nanosecond source/model accuracy.
#[derive(Clone, Serialize)]
pub struct EphemerisReferenceRow {
    /// Universal proleptic Gregorian midnight, with no misleading UTC suffix.
    pub reference_calendar: NaiveDateTime,
    /// UT1 before 1972; UTC from 1972. No timezone or birth-input resolution occurs.
    pub reference_scale: BirthTimeScale,
    /// Absent for historical UT1 labels.
    pub julian_day_utc: Option<f64>,
    /// TT coordinate relative to 2000-01-01T12:00:00 TT, in nanoseconds.
    pub tt_nanoseconds_since_j2000: i64,
    /// TDB coordinate relative to 2000-01-01T12:00:00 TDB, in nanoseconds.
    pub tdb_nanoseconds_since_j2000: i64,
    /// Actual floating-point TDB seconds passed to the native SPK evaluator.
    pub spk_tdb_seconds_since_j2000: f64,
    /// Explicit modeled Earth-rotation reading, retaining the provider's f64 precision.
    pub model_julian_day_ut1: f64,
    /// TT minus UT1 at this reference label, seconds.
    pub delta_t_seconds: f64,
    /// Historical model, observation, forecast or explicitly unbounded extrapolation.
    pub delta_t_kind: DeltaTKind,
    /// Quoted source error only, not an overall position/model error bound.
    pub delta_t_reported_error_seconds: Option<f64>,
    /// Public celestial state; node source ID, physical distance and range rate are null.
    #[serde(flatten)]
    pub position: BodyPosition,
}

impl EphemerisReferenceTable {
    /// Evaluate each eligible body at each civil midnight in `[from, to_exclusive)`.
    /// Reads only the checked-in DE440 source, once, after validating the bounded interval.
    ///
    /// # Errors
    ///
    /// Invalid intervals fail before source I/O. Kernel verification, source coverage and nonfinite
    /// calculation failures return errors rather than a partial table or substituted values.
    pub fn calculate(
        store: &dyn EphemerisStore,
        from: NaiveDate,
        to_exclusive: NaiveDate,
    ) -> Result<Self, ReferenceTableError> {
        validate_interval(from, to_exclusive)?;
        let mut manifest: EphemerisManifest =
            serde_json::from_str(include_str!("../manifest.json"))
                .map_err(|_| ReferenceTableError::InvalidSource)?;
        manifest.files.retain(|file| file.name == "de440.bsp");
        manifest.validate()?;
        if manifest.files.len() != 1 {
            return Err(ReferenceTableError::InvalidSource);
        }
        let loaded = crate::load(store, &manifest)?;
        let mut rows = Vec::new();
        let mut date = from;
        while date < to_exclusive {
            // Advance calendar labels, never fixed physical seconds across a UTC leap second.
            let instant = reference_instant(date)?;
            let scales = instant.time_scales();
            let epoch = instant.epoch();
            let tt = coordinate_nanoseconds(epoch, TimeScale::TT)?;
            let tdb = coordinate_nanoseconds(epoch, TimeScale::TDB)?;
            let argument = crate::utc_to_tdb(epoch);
            for body in BODIES {
                rows.push(EphemerisReferenceRow {
                    reference_calendar: scales.reference.calendar,
                    reference_scale: scales.reference.scale,
                    julian_day_utc: scales.julian_day_utc,
                    tt_nanoseconds_since_j2000: tt,
                    tdb_nanoseconds_since_j2000: tdb,
                    spk_tdb_seconds_since_j2000: argument.0,
                    model_julian_day_ut1: scales.model_julian_day_ut1,
                    delta_t_seconds: scales.delta_t_seconds,
                    delta_t_kind: scales.delta_t_kind,
                    delta_t_reported_error_seconds: scales.delta_t_reported_error_seconds,
                    position: crate::chart_provider::body_position(
                        &loaded.ephemeris,
                        body,
                        argument,
                    )?,
                });
            }
            date = date
                .succ_opt()
                .ok_or(ReferenceTableError::InvalidInterval)?;
        }
        Ok(Self {
            schema_version: 1,
            from,
            to_exclusive,
            rows,
            metadata: ReferenceMetadata {
                algorithm: crate::chart_provider::ALGORITHM,
                provider_version: env!("CARGO_PKG_VERSION"),
                source_manifest: loaded.manifest,
                delta_t_model: crate::lines::geojson::DELTA_T_MODEL,
                delta_t_sources: crate::delta_t_sources(),
                node_model: crate::lines::geojson::NODE_CONVENTION,
                zodiac: Zodiac::Tropical,
                origin: ObserverOrigin::Geocentric,
                frame: CoordinateFrame::TrueOfDate,
                frame_model: "IAU2006 precession/frame bias, IAU2000B nutation, true obliquity",
                north_node: NorthNode::Mean,
                spk_corrections: &["light_time", "annual_aberration"],
                gravitational_deflection: false,
                atmospheric_refraction: false,
                angular_unit: "degree",
                distance_unit: "kilometer",
                rate_time_unit: "86400 TDB-coordinate seconds",
                rate_time_scale: "TDB",
                rate_window_seconds: 2.0 * crate::chart_provider::RATE_STEP_SECONDS,
                tt_nanoseconds_origin: "2000-01-01T12:00:00 TT",
                tdb_nanoseconds_origin: "2000-01-01T12:00:00 TDB",
                spk_argument:
                    "hifitime 4.3.1 to_tdb_seconds, seconds since native J2000 TDB origin",
                interpolation_supported: false,
            },
        })
    }
}

fn validate_interval(from: NaiveDate, to: NaiveDate) -> Result<(), ReferenceTableError> {
    let days = to.signed_duration_since(from).num_days();
    if from.year() < 1900
        || to.year() > 2050
        || (to.year() == 2050 && (to.month() != 1 || to.day() != 1))
        || !(1..=MAX_REFERENCE_BATCH_DAYS).contains(&days)
    {
        return Err(ReferenceTableError::InvalidInterval);
    }
    Ok(())
}

fn reference_instant(date: NaiveDate) -> Result<ChartInstant, ChartError> {
    ChartInstant::from_reference(BirthTimeReference {
        calendar: date
            .and_hms_opt(0, 0, 0)
            .ok_or(ChartError::CalculationFailed)?,
        scale: if date.year() < 1972 {
            BirthTimeScale::Ut1
        } else {
            BirthTimeScale::Utc
        },
    })
}

fn coordinate_nanoseconds(epoch: Epoch, scale: TimeScale) -> Result<i64, ChartError> {
    // Gregorian construction avoids assuming TT and TDB share a physical origin or a library offset.
    let origin = Epoch::maybe_from_gregorian(2000, 1, 1, 12, 0, 0, 0, scale)
        .map_err(|_| ChartError::CalculationFailed)?;
    let coordinate =
        epoch.to_duration_in_time_scale(scale) - origin.to_duration_in_time_scale(scale);
    i64::try_from(coordinate.total_nanoseconds()).map_err(|_| ChartError::CalculationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_daily_midnight_retains_ut1_without_utc() {
        let instant = reference_instant(NaiveDate::from_ymd_opt(1900, 1, 1).unwrap()).unwrap();
        let scales = instant.time_scales();
        assert_eq!(
            scales.reference.scale,
            BirthTimeScale::Ut1,
            "daily reference rows must preserve the historical civil clock convention"
        );
        assert!(scales.julian_day_utc.is_none());
        assert_eq!(
            scales.model_julian_day_ut1.to_bits(),
            2_415_020.5_f64.to_bits()
        );
    }
    #[test]
    fn coordinates_use_each_scales_noon_and_preserve_exact_duration_readings() {
        use hifitime::Duration;
        let utc_noon =
            Epoch::maybe_from_gregorian(2000, 1, 1, 12, 0, 0, 0, TimeScale::UTC).unwrap();
        assert_eq!(
            coordinate_nanoseconds(utc_noon, TimeScale::TT).unwrap(),
            64_184_000_000
        );
        for scale in [TimeScale::TT, TimeScale::TDB] {
            let origin = Epoch::maybe_from_gregorian(2000, 1, 1, 12, 0, 0, 0, scale).unwrap();
            assert_eq!(coordinate_nanoseconds(origin, scale).unwrap(), 0);
            for date in ["1900-01-01", "1972-01-01", "2000-02-29", "2049-12-31"] {
                let epoch = reference_instant(date.parse().unwrap()).unwrap().epoch();
                let ns = coordinate_nanoseconds(epoch, scale).unwrap();
                let reading = origin.to_duration_in_time_scale(scale)
                    + Duration::from_total_nanoseconds(i128::from(ns));
                let reconstructed = Epoch::from_duration(reading, scale);
                assert_eq!(
                    reconstructed.to_duration_in_time_scale(scale),
                    epoch.to_duration_in_time_scale(scale)
                );
                assert_eq!(coordinate_nanoseconds(reconstructed, scale).unwrap(), ns);
            }
        }
        let tt_noon = Epoch::maybe_from_gregorian(2000, 1, 1, 12, 0, 0, 0, TimeScale::TT).unwrap();
        assert_ne!(
            coordinate_nanoseconds(tt_noon, TimeScale::TDB).unwrap(),
            0,
            "TT noon and TDB noon are different physical instants"
        );
    }

    #[test]
    fn calendar_midnights_retain_scale_transition_and_utc_leap_seconds() {
        let before = reference_instant("1971-12-31".parse().unwrap())
            .unwrap()
            .time_scales();
        let after = reference_instant("1972-01-01".parse().unwrap())
            .unwrap()
            .time_scales();
        assert_eq!(before.reference.scale, BirthTimeScale::Ut1);
        assert!(before.julian_day_utc.is_none());
        assert_eq!(after.reference.scale, BirthTimeScale::Utc);
        assert!(after.julian_day_utc.is_some());
        let midnight = |date: &str| {
            coordinate_nanoseconds(
                reference_instant(date.parse().unwrap()).unwrap().epoch(),
                TimeScale::TT,
            )
            .unwrap()
        };
        assert_eq!(
            midnight("2017-01-01") - midnight("2016-12-31"),
            86_401_000_000_000
        );
        assert_eq!(
            midnight("2017-01-02") - midnight("2017-01-01"),
            86_400_000_000_000
        );
        assert_eq!(
            midnight("2000-03-01") - midnight("2000-02-28"),
            2 * 86_400_000_000_000
        );
    }

    #[test]
    fn invalid_intervals_fail_before_source_io_and_batch_cap_is_not_dataset_coverage() {
        struct MissingSource(std::sync::atomic::AtomicUsize);
        impl EphemerisStore for MissingSource {
            fn read(&self, _: &str) -> Result<Vec<u8>, crate::EphemerisError> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(crate::EphemerisError::InvalidManifest {
                    problem: "synthetic missing source".into(),
                })
            }
        }
        let store = MissingSource(std::sync::atomic::AtomicUsize::new(0));
        for (from, to) in [
            ("2000-01-01", "2000-01-01"),
            ("2000-01-02", "2000-01-01"),
            ("1899-12-31", "1900-01-01"),
            ("2049-12-31", "2050-01-02"),
            ("2000-01-01", "2001-01-02"),
        ] {
            assert!(matches!(
                EphemerisReferenceTable::calculate(
                    &store,
                    from.parse().unwrap(),
                    to.parse().unwrap()
                ),
                Err(ReferenceTableError::InvalidInterval)
            ));
        }
        assert_eq!(store.0.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert!(matches!(
            EphemerisReferenceTable::calculate(
                &store,
                "2000-01-01".parse().unwrap(),
                "2001-01-01".parse().unwrap()
            ),
            Err(ReferenceTableError::Source(_))
        ));
        assert_eq!(store.0.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(
            validate_interval("2049-12-31".parse().unwrap(), "2050-01-01".parse().unwrap()).is_ok()
        );
    }
}
