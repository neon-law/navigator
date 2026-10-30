//! Typed errors for SPK parsing, interpolation, and kernel loading.

use std::path::PathBuf;

/// Everything that can go wrong while reading or evaluating ephemeris kernels.
#[derive(Debug, thiserror::Error)]
pub enum EphemerisError {
    /// The kernel file could not be read from the store.
    #[error("reading kernel {path}: {source}")]
    Io {
        /// Kernel name the store attempted to read.
        path: String,
        /// Underlying I/O failure.
        #[source]
        source: std::io::Error,
    },

    /// The file is not an SPK (identification word mismatch).
    #[error("{path} is not an SPK file: identification word {word:?}")]
    NotSpk {
        /// Kernel name.
        path: String,
        /// The eight bytes where `DAF/SPK ` was expected.
        word: String,
    },

    /// The DAF format string names neither supported IEEE endianness.
    #[error("{path} declares unsupported DAF format {format:?}")]
    UnsupportedFormat {
        /// Kernel name.
        path: String,
        /// The eight-byte LOCFMT field.
        format: String,
    },

    /// The file ends before a DAF structure it references.
    #[error("{path} is truncated: {needed} bytes needed, {available} available")]
    Truncated {
        /// Kernel name.
        path: String,
        /// Bytes required by the referenced structure.
        needed: usize,
        /// Bytes actually present.
        available: usize,
    },

    /// A segment declares a structure inconsistent with its address range.
    #[error("segment {target}->{center} in {path}: {problem}")]
    MalformedSegment {
        /// Kernel name.
        path: String,
        /// Segment target body.
        target: i32,
        /// Segment center body.
        center: i32,
        /// What failed validation.
        problem: String,
    },

    /// A segment uses an SPK data type this crate does not interpolate.
    #[error("segment {target}->{center} in {path} uses unsupported SPK data type {spk_type}")]
    UnsupportedSpkType {
        /// Kernel name.
        path: String,
        /// Segment target body.
        target: i32,
        /// Segment center body.
        center: i32,
        /// The SPK data type number.
        spk_type: i32,
    },

    /// A segment is expressed in a frame other than J2000/ICRF (frame 1).
    #[error("segment {target}->{center} in {path} uses unsupported frame {frame}")]
    UnsupportedFrame {
        /// Kernel name.
        path: String,
        /// Segment target body.
        target: i32,
        /// Segment center body.
        center: i32,
        /// The NAIF frame id.
        frame: i32,
    },

    /// No loaded file carries the requested body.
    #[error("no ephemeris segment carries body {naif_id}")]
    UnknownBody {
        /// The NAIF id requested.
        naif_id: i32,
    },

    /// The requested time falls outside every segment carrying the body.
    #[error("body {naif_id} is not covered at TDB {tdb_seconds:.0} (coverage {coverage_start:.0}..{coverage_end:.0})")]
    OutsideCoverage {
        /// The NAIF id requested.
        naif_id: i32,
        /// Requested TDB seconds past J2000.
        tdb_seconds: f64,
        /// First covered TDB second.
        coverage_start: f64,
        /// Last covered TDB second.
        coverage_end: f64,
    },

    /// Body resolution recursed past the link-depth guard (a segment cycle).
    #[error("body resolution exceeded {depth} links near body {naif_id}")]
    ResolutionOverflow {
        /// Depth at which the guard fired.
        depth: u8,
        /// Body being resolved when the guard fired.
        naif_id: i32,
    },

    /// A difference line declares interpolation orders outside the record.
    #[error("difference line for {target}->{center} in {path}: {problem}")]
    MalformedDifferenceLine {
        /// Kernel name.
        path: String,
        /// Segment target body.
        target: i32,
        /// Segment center body.
        center: i32,
        /// What failed validation.
        problem: String,
    },

    /// The manifest digest does not match the kernel bytes.
    #[error("checksum mismatch for {name}: manifest pins {expected}, file hashes to {actual}")]
    ChecksumMismatch {
        /// Kernel name.
        name: String,
        /// SHA-256 recorded in the manifest.
        expected: String,
        /// SHA-256 of the bytes actually read.
        actual: String,
    },

    /// The manifest references a file it does not describe, or lists nothing.
    #[error("invalid ephemeris manifest: {problem}")]
    InvalidManifest {
        /// What failed validation.
        problem: String,
    },

    /// Serializing derived output failed, most often a non-finite
    /// coordinate that would corrupt geometry as JSON `null`.
    #[error("serializing derived output: {problem}")]
    Serialization {
        /// What failed and where.
        problem: String,
    },

    /// The configured filesystem data root does not exist.
    #[error("ephemeris data directory {} does not exist", .0.display())]
    MissingDataDir(PathBuf),

    /// Light-time iteration did not converge within the pass cap.
    #[error("light-time iteration for body {naif_id} did not converge within {iterations} passes")]
    LightTimeNoConvergence {
        /// NAIF id of the body whose light time failed to converge.
        naif_id: i32,
        /// The iteration cap that was reached.
        iterations: usize,
    },
}
