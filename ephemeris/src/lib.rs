//! Barycentric positions for the astrocartography calculator.
//!
//! The crate reads NASA/JPL SPK files (`de440.bsp` plus a Horizons-generated
//! small-body SPK for Chiron) and interpolates ICRF/J2000 barycentric positions
//! in kilometers. Civil UTC birth instants convert to TDB seconds past J2000
//! through [`hifitime`]: UTC→TAI applies the built-in leap-second table, TT =
//! TAI + 32.184 s, and TDB = TT + the periodic ~1.6 ms relativistic term. The
//! classical ΔT (TT−UT1) is a property of Earth rotation, not of the ephemeris
//! lookup, and enters only later layers that compute angles from Earth
//! orientation.
//!
//! The kernel files live outside the repository. [`loader`] pins their bytes
//! with SHA-256 checksums recorded in a checked-in manifest, and exposes the
//! parsed, immutable [`Ephemeris`] for parking in shared application state.

pub mod apparent;

mod daf;
mod error;
mod loader;
mod spk;
mod time;

pub use error::EphemerisError;
pub use loader::{
    load, EphemerisFile, EphemerisManifest, EphemerisStore, FilesystemStore, LoadedEphemeris,
    DATA_DIR_ENV,
};
pub use spk::{Body, Ephemeris, SpkFile};
pub use time::{utc_to_tdb, utc_to_tdb_from_parts, TdbSeconds};
