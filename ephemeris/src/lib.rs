//! Barycentric positions for the astrocartography calculator.
//!
//! The crate reads NASA/JPL SPK files (`de440.bsp` plus a Horizons-generated
//! small-body SPK for Chiron) and interpolates ICRF/J2000 barycentric positions
//! in kilometers. Resolved civil birth clocks before 1972 are interpreted as UT1;
//! TT follows by adding the pinned delta-T model. From 1972 onward UTC converts
//! through hifitime's pinned leap-second table to TAI, then TT. Both paths use
//! TT plus the periodic relativistic term for the SPK's TDB argument. Earth
//! rotation retains explicit UT1 rather than reinterpreting the physical epoch.
//!
//! The kernel files live outside the repository. [`loader`] pins their bytes
//! with SHA-256 checksums recorded in a checked-in manifest, and exposes the
//! parsed, immutable [`Ephemeris`] for parking in shared application state.

pub mod apparent;
pub mod lines;

mod astrocartography;
mod birth_time;
mod chart_angles;
mod chart_provider;
mod chart_request;
mod chart_time;
mod daf;
mod error;
mod loader;
mod spk;
mod time;

pub use apparent::{
    apparent_geocentric, apparent_geocentric_state, mean_node, ApparentPosition, ApparentState,
};
pub use error::EphemerisError;
pub use loader::{
    load, EphemerisFile, EphemerisManifest, EphemerisStore, FilesystemStore, LoadedEphemeris,
    DATA_DIR_ENV, MANIFEST_VERSION,
};
pub use spk::{Body, Ephemeris, SpkFile};
pub use time::{utc_to_tdb, utc_to_tdb_from_parts, TdbSeconds};

pub use birth_time::{
    BirthTime, BirthTimeError, BirthTimeReference, BirthTimeScale, LocalBirthTime,
    ResolvedBirthTime,
};

pub use chart_request::{
    AstrologyChartRequest, AtmosphericRefraction, Ayanamsa, ChartError, ChartSettings,
    CoordinateFrame, HouseSystem, NorthNode, ObserverOrigin, Zodiac,
};

pub use chart_angles::ChartAngles;

pub use chart_provider::{
    AstrologyChartProvider, AstrologyChartResponse, BodyPosition, ChartProvenance,
};

pub use chart_time::{ChartInstant, ChartTimeScales};

pub use lines::deltat::{
    delta_t_estimate, delta_t_sources, DeltaTDataFile, DeltaTEstimate, DeltaTKind,
};

pub use astrocartography::{
    AstrocartographyGeometry, GeographicLine, GeometryConvention, GeometryPointRow,
    GeometryProvenance,
};
