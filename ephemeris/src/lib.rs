//! Barycentric positions for the astrocartography calculator.
//!
//! The crate reads NASA/JPL SPK files (`de440.bsp` plus a Horizons-generated
//! small-body SPK for Chiron) and interpolates ICRF/J2000 barycentric positions
//! in kilometers.

mod daf;
mod error;
mod spk;

pub use error::EphemerisError;
pub use spk::{Body, Ephemeris, SpkFile};
