//! DAF (Double precision Array File) low-level record access.
//!
//! An SPK file is a DAF. The file record (first 1024 bytes) locates the first
//! and last summary records; summary records chain their segments as packed
//! descriptors. All real values live in 8-byte IEEE-754 "words" addressed
//! 1-based across the whole file, in the endianness the file's LOCFMT field
//! declares. DE440 ships little-endian; the reader honors both IEEE orders so
//! NAIF big-endian kernels parse identically.

use crate::error::EphemerisError;

/// DAF records are 1024 bytes.
pub(crate) const RECORD_BYTES: usize = 1024;

/// The SPK identification word.
const SPK_MAGIC: &[u8; 8] = b"DAF/SPK ";

/// IEEE endianness declared by a DAF's LOCFMT field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Endian {
    /// `LTL-IEEE` — least-significant byte first.
    Little,
    /// `MSB-IEEE` — most-significant byte first.
    Big,
}

impl Endian {
    fn f64_from(self, bytes: &[u8]) -> f64 {
        let mut array = [0_u8; 8];
        array.copy_from_slice(bytes);
        match self {
            Endian::Little => f64::from_le_bytes(array),
            Endian::Big => f64::from_be_bytes(array),
        }
    }

    fn i32_from(self, bytes: &[u8]) -> i32 {
        let mut array = [0_u8; 4];
        array.copy_from_slice(bytes);
        match self {
            Endian::Little => i32::from_le_bytes(array),
            Endian::Big => i32::from_be_bytes(array),
        }
    }
}

/// An opened DAF: owned bytes plus the file-record pointers.
#[derive(Debug)]
pub(crate) struct DafFile {
    /// The kernel name for error reporting.
    name: String,
    /// The complete file bytes.
    bytes: Vec<u8>,
    /// Endianness from LOCFMT.
    endian: Endian,
    /// Record number of the first summary record chain (FWARD).
    first_summary_record: i32,
}

impl DafFile {
    /// Validates the file record and captures the summary-chain head.
    ///
    /// # Errors
    ///
    /// Returns [`EphemerisError::NotSpk`] for a foreign identification word,
    /// [`EphemerisError::UnsupportedFormat`] for a non-IEEE LOCFMT, and
    /// [`EphemerisError::Truncated`] when the file is under one record.
    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Result<Self, EphemerisError> {
        let name = "<kernel>".to_string();
        let file_len = bytes.len();
        if file_len < RECORD_BYTES {
            return Err(EphemerisError::Truncated {
                path: name.clone(),
                needed: RECORD_BYTES,
                available: file_len,
            });
        }
        if &bytes[0..8] != SPK_MAGIC {
            return Err(EphemerisError::NotSpk {
                path: name.clone(),
                word: String::from_utf8_lossy(&bytes[0..8]).into_owned(),
            });
        }
        // LOCFMT is pure ASCII, so it reads correctly before endianness is known.
        let endian = match &bytes[88..96] {
            b"LTL-IEEE" => Endian::Little,
            b"MSB-IEEE" => Endian::Big,
            other => {
                return Err(EphemerisError::UnsupportedFormat {
                    path: name.clone(),
                    format: String::from_utf8_lossy(other).into_owned(),
                });
            }
        };
        // FWARD lives at bytes 76..80 (fortran words 19-20 packed as two i32).
        let first_summary_record = Self::raw_i32(&bytes, endian, 76);
        Ok(Self {
            name,
            bytes,
            endian,
            first_summary_record,
        })
    }

    /// Attaches the kernel's display name for error messages.
    pub(crate) fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// The kernel name.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    fn raw_i32(bytes: &[u8], endian: Endian, offset: usize) -> i32 {
        endian.i32_from(&bytes[offset..offset + 4])
    }

    /// The 1-based 8-byte word as an IEEE double.
    ///
    /// # Errors
    ///
    /// Returns [`EphemerisError::Truncated`] when the word lies past EOF.
    pub(crate) fn f64_word(&self, word: i64) -> Result<f64, EphemerisError> {
        let offset = (word - 1)
            .checked_mul(8)
            .ok_or_else(|| EphemerisError::MalformedSegment {
                path: self.name.clone(),
                target: 0,
                center: 0,
                problem: format!("word address {word} overflows"),
            })?;
        let end = usize::try_from(offset).map_err(|_| EphemerisError::MalformedSegment {
            path: self.name.clone(),
            target: 0,
            center: 0,
            problem: format!("word address {word} overflows"),
        })? + 8;
        if end > self.bytes.len() {
            return Err(EphemerisError::Truncated {
                path: self.name.clone(),
                needed: end,
                available: self.bytes.len(),
            });
        }
        Ok(self.endian.f64_from(&self.bytes[(end - 8)..end]))
    }

    /// The two packed 4-byte integers inside a DAF word (FORTRAN
    /// `2I*4`-style packing used by summary descriptors).
    ///
    /// # Errors
    ///
    /// Returns [`EphemerisError::Truncated`] when the word lies past EOF.
    pub(crate) fn int_pair(&self, word: i64) -> Result<(i32, i32), EphemerisError> {
        let offset = (word - 1)
            .checked_mul(8)
            .ok_or_else(|| EphemerisError::MalformedSegment {
                path: self.name.clone(),
                target: 0,
                center: 0,
                problem: format!("word address {word} overflows"),
            })?;
        let start = usize::try_from(offset).map_err(|_| EphemerisError::MalformedSegment {
            path: self.name.clone(),
            target: 0,
            center: 0,
            problem: format!("word address {word} overflows"),
        })?;
        if start + 8 > self.bytes.len() {
            return Err(EphemerisError::Truncated {
                path: self.name.clone(),
                needed: start + 8,
                available: self.bytes.len(),
            });
        }
        Ok((
            self.endian.i32_from(&self.bytes[start..start + 4]),
            self.endian.i32_from(&self.bytes[start + 4..start + 8]),
        ))
    }

    /// The record number where the summary chain begins (FWARD).
    pub(crate) fn first_summary_record(&self) -> i32 {
        self.first_summary_record
    }

    /// The words-per-summary stride: ND doubles plus the packed integer words.
    pub(crate) fn summary_words(nd: i32, ni: i32) -> i64 {
        i64::from(nd) + (i64::from(ni) + 1) / 2
    }

    /// The raw kernel bytes (test-only corruption patching).
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

// Synthetic fixtures are built so evaluations reproduce the expected values
// bit-exactly; a tolerance would weaken these assertions.
#[allow(clippy::float_cmp)]
#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal file record: magic, ND=2, NI=6, FWARD=2, LOCFMT LE.
    fn minimal_file_record() -> Vec<u8> {
        let mut record = vec![0_u8; RECORD_BYTES];
        record[0..8].copy_from_slice(SPK_MAGIC);
        record[8..12].copy_from_slice(&2_i32.to_le_bytes());
        record[12..16].copy_from_slice(&6_i32.to_le_bytes());
        record[76..80].copy_from_slice(&2_i32.to_le_bytes());
        record[80..84].copy_from_slice(&2_i32.to_le_bytes());
        record[84..88].copy_from_slice(&8_i32.to_le_bytes());
        record[88..96].copy_from_slice(b"LTL-IEEE");
        record
    }

    #[test]
    fn parses_little_endian_file_record() {
        let daf = DafFile::from_bytes(minimal_file_record()).expect("valid record");
        assert_eq!(daf.first_summary_record(), 2);
        assert_eq!(DafFile::summary_words(2, 6), 5);
    }

    #[test]
    fn rejects_non_spk_magic() {
        let mut record = minimal_file_record();
        record[0..8].copy_from_slice(b"DAF/SPKX");
        let error = DafFile::from_bytes(record).expect_err("must reject");
        assert!(matches!(error, EphemerisError::NotSpk { .. }));
    }

    #[test]
    fn rejects_unsupported_format() {
        let mut record = minimal_file_record();
        record[88..96].copy_from_slice(b"XTL-IEEE");
        let error = DafFile::from_bytes(record).expect_err("must reject");
        assert!(matches!(error, EphemerisError::UnsupportedFormat { .. }));
    }

    #[test]
    fn rejects_undersized_file() {
        let error = DafFile::from_bytes(vec![0_u8; 512]).expect_err("must reject");
        assert!(matches!(error, EphemerisError::Truncated { .. }));
    }

    #[test]
    fn reads_words_in_declared_endianness() {
        let mut record = minimal_file_record();
        // DAF words are addressed globally, 1-based across the whole file:
        // word 129 begins at byte (129−1)·8. Write 3.5 there in big endian,
        // with a big-endian LOCFMT to match.
        record[88..96].copy_from_slice(b"MSB-IEEE");
        let offset = (129 - 1) * 8;
        record.resize(offset + 8, 0);
        record[offset..offset + 8].copy_from_slice(&3.5_f64.to_be_bytes());
        let daf = DafFile::from_bytes(record).expect("valid record");
        let value = daf.f64_word(129).expect("in bounds");
        assert_eq!(value, 3.5);
    }

    #[test]
    fn reads_packed_integer_pairs() {
        let mut record = minimal_file_record();
        // Word 129 at global byte (129−1)·8; the two 4-byte halves unpack as
        // a pair of integers.
        let offset = (129 - 1) * 8;
        record.resize(offset + 8, 0);
        record[offset..offset + 4].copy_from_slice(&3_i32.to_le_bytes());
        record[offset + 4..offset + 8].copy_from_slice(&(-7_i32).to_le_bytes());
        let daf = DafFile::from_bytes(record).expect("valid record");
        assert_eq!(daf.int_pair(129).expect("in bounds"), (3, -7));
    }

    #[test]
    fn out_of_bounds_word_is_truncated_error() {
        let daf = DafFile::from_bytes(minimal_file_record()).expect("valid record");
        let error = daf.f64_word(10_000).expect_err("out of bounds");
        assert!(matches!(error, EphemerisError::Truncated { .. }));
    }
}
