//! Physical evaluation epochs derived from explicitly scaled civil clocks.

use crate::{BirthTimeReference, BirthTimeScale, ChartError};
use chrono::{Datelike, Timelike};
use hifitime::{Duration, Epoch, TimeScale};
use serde::Serialize;

const DAY_SECONDS: f64 = 86_400.0;

/// Distinct time-scale readings, retaining the original universal civil reference.
#[derive(Clone, Serialize)]
pub struct ChartTimeScales {
    /// Normalized civil calendar label and its scale.
    pub reference: BirthTimeReference,
    /// Civil UTC Julian day; absent for historical UT1 input.
    pub julian_day_utc: Option<f64>,
    /// Terrestrial Time Julian day.
    pub julian_day_tt: f64,
    /// Barycentric Dynamical Time Julian day used by SPK interpolation.
    pub julian_day_tdb: f64,
    /// Earth-rotation UT1 reading; the supplied calendar for historical input.
    pub model_julian_day_ut1: f64,
    /// Model TT minus UT1, seconds, evaluated at the universal civil calendar.
    pub delta_t_seconds: f64,
    /// Observation, forecast, or explicitly unbounded extrapolation.
    pub delta_t_kind: crate::DeltaTKind,
    /// Quoted source error, not a guaranteed model/interpolation bound.
    pub delta_t_reported_error_seconds: Option<f64>,
}

/// One physical epoch paired with its explicit Earth-rotation reading.
/// Personal calculation values have no debug formatting.
#[derive(Clone, Copy)]
pub struct ChartInstant {
    epoch: Epoch,
    ut1: f64,
    reference: BirthTimeReference,
    delta_t: crate::DeltaTEstimate,
}

impl ChartInstant {
    pub(crate) fn from_reference(reference: BirthTimeReference) -> Result<Self, ChartError> {
        let calendar = reference.calendar;
        let component = |value| u8::try_from(value).map_err(|_| ChartError::CalculationFailed);
        let scale = match reference.scale {
            BirthTimeScale::Ut1 => TimeScale::TT,
            BirthTimeScale::Utc => TimeScale::UTC,
        };
        let reading = Epoch::maybe_from_gregorian(
            calendar.year(),
            component(calendar.month())?,
            component(calendar.day())?,
            component(calendar.hour())?,
            component(calendar.minute())?,
            component(calendar.second())?,
            calendar.nanosecond(),
            scale,
        )
        .map_err(|_| ChartError::CalculationFailed)?;
        let calendar_jd = match reference.scale {
            BirthTimeScale::Ut1 => reading.to_jde_tt_days(),
            BirthTimeScale::Utc => reading.to_jde_utc_days(),
        };
        let delta_t = crate::delta_t_estimate(calendar_jd).ok_or(ChartError::CalculationFailed)?;
        let (epoch, ut1) = match reference.scale {
            BirthTimeScale::Ut1 => (
                reading + Duration::from_seconds(delta_t.seconds),
                calendar_jd,
            ),
            BirthTimeScale::Utc => (
                reading,
                reading.to_jde_tt_days() - delta_t.seconds / DAY_SECONDS,
            ),
        };
        Ok(Self {
            epoch,
            ut1,
            reference,
            delta_t,
        })
    }

    /// Physical instant for orbital interpolation and TT frame arguments.
    #[must_use]
    pub fn epoch(self) -> Epoch {
        self.epoch
    }

    /// Explicit UT1 Julian day for Earth rotation, without reinterpreting the epoch as UTC.
    #[must_use]
    pub fn ut1_julian_day(self) -> f64 {
        self.ut1
    }

    /// Reproducible readings and original scale; historical input invents no UTC reading.
    #[must_use]
    pub fn time_scales(self) -> ChartTimeScales {
        ChartTimeScales {
            reference: self.reference,
            julian_day_utc: (self.reference.scale == BirthTimeScale::Utc)
                .then(|| self.epoch.to_jde_utc_days()),
            julian_day_tt: self.epoch.to_jde_tt_days(),
            julian_day_tdb: 2_451_545.0 + self.epoch.to_tdb_seconds() / DAY_SECONDS,
            model_julian_day_ut1: self.ut1,
            delta_t_seconds: self.delta_t.seconds,
            delta_t_kind: self.delta_t.kind,
            delta_t_reported_error_seconds: self.delta_t.reported_error_seconds,
        }
    }
}
