//! Kernel loading: a manifest pins bytes, a store abstracts location, and
//! [`load`] produces the immutable [`Ephemeris`].
//!
//! The kernel files stay outside the repository (119 MB of DE440 does not
//! belong in git). The checked-in manifest records each file's name, SHA-256,
//! size, source URL, and coverage; the loader refuses any file whose bytes do
//! not hash to the pinned digest before parsing it. Storage behind the files
//! is still an open decision, so reads go through the [`EphemerisStore`]
//! trait — [`FilesystemStore`] serves development and tests, and an
//! object-store implementation can arrive later without touching callers.

use crate::error::EphemerisError;
use crate::spk::{Ephemeris, SpkFile};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// The environment variable naming the filesystem data root.
pub const DATA_DIR_ENV: &str = "NAVIGATOR_EPHEMERIS_DIR";

/// Reads kernel bytes by manifest name.
pub trait EphemerisStore: Send + Sync {
    /// Returns the complete bytes of the named kernel.
    ///
    /// # Errors
    ///
    /// Returns [`EphemerisError::Io`] when the store cannot deliver the bytes.
    fn read(&self, name: &str) -> Result<Vec<u8>, EphemerisError>;
}

/// Filesystem-backed store resolving names under a data root directory.
pub struct FilesystemStore {
    root: PathBuf,
}

impl FilesystemStore {
    /// A store rooted at the given directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// A store rooted at [`DATA_DIR_ENV`].
    ///
    /// # Errors
    ///
    /// Returns [`EphemerisError::MissingDataDir`] when the variable is unset
    /// or its directory does not exist.
    pub fn from_env() -> Result<Self, EphemerisError> {
        let root = std::env::var_os(DATA_DIR_ENV)
            .map(PathBuf::from)
            .ok_or_else(|| {
                EphemerisError::MissingDataDir(PathBuf::from(format!(
                    "${DATA_DIR_ENV} (unset; point it at the ephemeris data directory)"
                )))
            })?;
        if !root.is_dir() {
            return Err(EphemerisError::MissingDataDir(root));
        }
        Ok(Self { root })
    }
}

impl EphemerisStore for FilesystemStore {
    fn read(&self, name: &str) -> Result<Vec<u8>, EphemerisError> {
        // Names come from the checked-in manifest, never from user input; the
        // file-name component check keeps a future manifest mistake from
        // escaping the data root.
        if PathBuf::from(name)
            .file_name()
            .is_none_or(|base| base != name)
        {
            return Err(EphemerisError::InvalidManifest {
                problem: format!("kernel name {name:?} is not a bare file name"),
            });
        }
        std::fs::read(self.root.join(name)).map_err(|source| EphemerisError::Io {
            path: name.to_string(),
            source,
        })
    }
}

/// One pinned kernel file in the manifest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EphemerisFile {
    /// Bare file name inside the data root, for example `de440.bsp`.
    pub name: String,
    /// SHA-256 of the file bytes, lowercase hex.
    pub sha256: String,
    /// Exact size in bytes.
    pub bytes: u64,
    /// Where the file was downloaded from.
    pub source: String,
    /// Human-readable coverage start (TDB), for example `1549-12-21`.
    pub coverage_start: String,
    /// Human-readable coverage end (TDB), for example `2650-01-25`.
    pub coverage_end: String,
}

/// The checked-in manifest pinning every kernel the crate may load.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EphemerisManifest {
    /// Manifest format version, currently 1.
    pub version: u32,
    /// The ΔT / time-scale model used to convert UTC birth data to TDB, kept
    /// verbatim in the provenance output for reproducibility.
    pub time_scale_model: String,
    /// The reference frame every segment is expressed in.
    pub reference_frame: String,
    /// The pinned files, loaded in order.
    pub files: Vec<EphemerisFile>,
}

/// The loaded ephemeris plus the manifest that proves its provenance.
#[derive(Debug)]
pub struct LoadedEphemeris {
    /// The parsed, immutable ephemeris ready for shared application state.
    pub ephemeris: Ephemeris,
    /// The manifest whose checksums verified every file.
    pub manifest: EphemerisManifest,
}

/// Loads and verifies every manifest file, then parses them into one
/// [`Ephemeris`]. Intended to run once at boot and park the result in shared
/// immutable state.
///
/// # Errors
///
/// Returns checksum errors for tampered files, I/O errors from the store, and
/// format errors from unparseable kernels.
pub fn load(
    store: &dyn EphemerisStore,
    manifest: &EphemerisManifest,
) -> Result<LoadedEphemeris, EphemerisError> {
    if manifest.files.is_empty() {
        return Err(EphemerisError::InvalidManifest {
            problem: "no files listed".to_string(),
        });
    }
    let mut seen = std::collections::HashSet::new();
    let mut files = Vec::with_capacity(manifest.files.len());
    for entry in &manifest.files {
        if !seen.insert(entry.name.clone()) {
            return Err(EphemerisError::InvalidManifest {
                problem: format!("duplicate file {}", entry.name),
            });
        }
        let bytes = store.read(&entry.name)?;
        verify_checksum(&entry.name, &entry.sha256, &bytes)?;
        files.push(SpkFile::from_bytes(bytes, entry.name.clone())?);
    }
    Ok(LoadedEphemeris {
        ephemeris: Ephemeris::new(files),
        manifest: manifest.clone(),
    })
}

/// Lowercase hex encoding, for manifest digest comparisons.
fn hex_encode(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        hex.push(char::from(HEX_DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(HEX_DIGITS[usize::from(byte & 0x0F)]));
    }
    hex
}

/// Computes the SHA-256 of `bytes` and refuses any deviation from the pinned
/// lowercase-hex digest.
fn verify_checksum(name: &str, expected: &str, bytes: &[u8]) -> Result<(), EphemerisError> {
    let digest = Sha256::digest(bytes);
    let actual = hex_encode(&digest);
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(EphemerisError::ChecksumMismatch {
            name: name.to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}

// Synthetic fixtures are built so evaluations reproduce the expected values
// bit-exactly; a tolerance would weaken these assertions.
#[allow(clippy::float_cmp)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::spk::test_support::synthetic_type2;
    use tempfile::TempDir;

    const DAY: f64 = 86_400.0;

    /// In-memory store for harness runs that need no disk.
    struct BytesStore(Vec<u8>);

    impl EphemerisStore for BytesStore {
        fn read(&self, _name: &str) -> Result<Vec<u8>, EphemerisError> {
            Ok(self.0.clone())
        }
    }

    fn hex_digest(bytes: &[u8]) -> String {
        let digest = Sha256::digest(bytes);
        hex_encode(&digest)
    }

    fn manifest_for(name: &str, sha256: String, size: usize) -> EphemerisManifest {
        EphemerisManifest {
            version: 1,
            time_scale_model: "UTC→TAI via hifitime leap-second table; TT = TAI + 32.184 s; TDB = TT + periodic term".to_string(),
            reference_frame: "ICRF / J2000 (NAIF frame 1)".to_string(),
            files: vec![EphemerisFile {
                name: name.to_string(),
                sha256,
                bytes: size as u64,
                source: "synthetic://test".to_string(),
                coverage_start: "2000-01-01".to_string(),
                coverage_end: "2000-01-02".to_string(),
            }],
        }
    }

    /// The required proof: a tampered kernel is refused before parsing.
    #[test]
    fn checksum_mismatch_is_refused() {
        let kernel = synthetic_type2(
            3,
            0,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[1.0; 2][..],
                &[1.0; 2][..],
                &[1.0; 2][..],
            )],
        );
        let mut bytes = kernel.raw_bytes_for_test();
        // The manifest pins the intact file's digest; flipping one bit inside
        // the Chebyshev coefficients changes the hash, so the loader must
        // refuse the kernel outright.
        let intact_digest = hex_digest(&bytes);
        bytes[3050] ^= 0xFF; // inside the segment data, well before EOF
        let manifest = manifest_for("kernel.bsp", intact_digest, bytes.len());
        let store = BytesStore(bytes);
        let error = load(&store, &manifest).expect_err("checksum must fail");
        assert!(matches!(error, EphemerisError::ChecksumMismatch { .. }));
    }

    /// A matching checksum loads and the parsed ephemeris interpolates.
    #[test]
    fn matching_checksum_loads_and_interpolates() {
        let kernel = synthetic_type2(
            3,
            0,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[1000.0; 2][..],
                &[2000.0; 2][..],
                &[3000.0; 2][..],
            )],
        );
        let bytes = kernel.raw_bytes_for_test();
        let manifest = manifest_for("kernel.bsp", hex_digest(&bytes), bytes.len());
        let loaded = load(&BytesStore(bytes), &manifest).expect("valid kernel");
        let position = loaded
            .ephemeris
            .barycentric_position_id(3, crate::time::TdbSeconds(DAY / 2.0))
            .expect("covered");
        assert_eq!(position, [1000.0, 2000.0, 3000.0]);
    }

    /// The filesystem store resolves names under its root.
    #[test]
    fn filesystem_store_reads_from_root() {
        let root = TempDir::new().expect("temp dir");
        std::fs::write(root.path().join("kernel.bsp"), b"kernel-bytes").expect("write");
        let store = FilesystemStore::new(root.path());
        let bytes = store.read("kernel.bsp").expect("in root");
        assert_eq!(bytes, b"kernel-bytes");
    }

    /// Traversal names are rejected before touching the filesystem.
    #[test]
    fn filesystem_store_rejects_paths() {
        let store = FilesystemStore::new(".");
        let error = store.read("sub/dir/kernel.bsp").expect_err("must refuse");
        assert!(matches!(error, EphemerisError::InvalidManifest { .. }));
    }

    /// Duplicate manifest entries are refused.
    #[test]
    fn duplicate_manifest_entries_are_refused() {
        // A real, verifying kernel lets the first entry pass read+verify+parse
        // so the loop reaches the duplicate check on the second entry.
        let kernel = synthetic_type2(
            3,
            0,
            0.0,
            DAY,
            &[(
                DAY / 2.0,
                DAY / 2.0,
                &[1.0; 2][..],
                &[1.0; 2][..],
                &[1.0; 2][..],
            )],
        );
        let bytes = kernel.raw_bytes_for_test();
        let entry = EphemerisFile {
            name: "kernel.bsp".to_string(),
            sha256: hex_digest(&bytes),
            bytes: bytes.len() as u64,
            source: String::new(),
            coverage_start: String::new(),
            coverage_end: String::new(),
        };
        let manifest = EphemerisManifest {
            version: 1,
            time_scale_model: String::new(),
            reference_frame: String::new(),
            files: vec![entry.clone(), entry],
        };
        let error = load(&BytesStore(bytes), &manifest).expect_err("must refuse");
        assert!(matches!(error, EphemerisError::InvalidManifest { .. }));
    }
}
