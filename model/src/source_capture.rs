//! Select and verify sections of a captured operational export without a storage connection.

use std::io::{self, Read};

use sha2::{Digest, Sha256};

use std::collections::BTreeSet;

/// Select the sections to keep in a buffered or streaming export.
pub struct Sections {
    derived: BTreeSet<String>,
    skipping: bool,
}

impl Sections {
    /// Start at the beginning of an export, omitting the given table names.
    pub fn new(derived: impl IntoIterator<Item = String>) -> Self {
        Self {
            derived: derived.into_iter().collect(),
            skipping: false,
        }
    }

    /// Advance to the next complete line and report whether to keep it.
    pub fn retain(&mut self, line: &str) -> bool {
        let header = line
            .trim_end()
            .strip_prefix("-- ")
            .filter(|header| !header.starts_with('-'));
        if let Some(header) = header {
            self.skipping = header
                .strip_prefix("TABLE DATA: ")
                .or_else(|| header.strip_prefix("TABLE: "))
                .is_some_and(|name| self.derived.contains(name));
        }
        !self.skipping
    }
}

/// An unexpected banner cannot consume unbounded memory during verification.
#[derive(Debug, thiserror::Error)]
#[error("operational export contains an unsupported section banner")]
pub struct FilterError;

/// Stream payload rows directly; retain only one bounded section banner.
pub struct ExportFilter {
    sections: Sections,
    prefix: Vec<u8>,
    content: bool,
}

impl ExportFilter {
    /// Use the same derived-table set as the canonical buffered export.
    pub fn new(derived: impl IntoIterator<Item = String>) -> Self {
        Self {
            sections: Sections::new(derived),
            prefix: Vec::with_capacity(256),
            content: false,
        }
    }

    /// Feed one transport chunk without retaining its payload rows.
    ///
    /// # Errors
    /// The export has a malformed or oversized section banner.
    pub fn feed(
        &mut self,
        mut bytes: &[u8],
        emit: &mut impl FnMut(&[u8]),
    ) -> Result<(), FilterError> {
        while !bytes.is_empty() {
            if self.content {
                let newline = bytes.iter().position(|byte| *byte == b'\n');
                let end = newline.map_or(bytes.len(), |index| index + 1);
                if !self.sections.skipping {
                    emit(&bytes[..end]);
                }
                bytes = &bytes[end..];
                if newline.is_some() {
                    self.content = false;
                }
                continue;
            }
            if self.prefix.len() == 256 {
                return Err(FilterError);
            }
            let byte = bytes[0];
            bytes = &bytes[1..];
            self.prefix.push(byte);
            if byte == b'\n' {
                self.flush_header(emit)?;
            } else if (self.prefix.len() <= 3 && !b"-- ".starts_with(&self.prefix))
                || (self.prefix.len() == 4 && byte == b'-')
            {
                if !self.sections.skipping {
                    emit(&self.prefix);
                }
                self.prefix.clear();
                self.content = true;
            }
        }
        Ok(())
    }

    /// Finish a final banner without a newline. Prior content has already streamed.
    ///
    /// # Errors
    /// The final banner has invalid UTF-8.
    pub fn finish(&mut self, emit: &mut impl FnMut(&[u8])) -> Result<(), FilterError> {
        if !self.prefix.is_empty() {
            self.flush_header(emit)?;
        }
        Ok(())
    }

    fn flush_header(&mut self, emit: &mut impl FnMut(&[u8])) -> Result<(), FilterError> {
        let header = std::str::from_utf8(&self.prefix).map_err(|_| FilterError)?;
        if self.sections.retain(header) {
            emit(&self.prefix);
        }
        self.prefix.clear();
        Ok(())
    }
}

/// Count and hash the exact bytes of an operational capture.
#[derive(Debug, Default)]
pub struct Fingerprint {
    bytes: u64,
    hash: Sha256,
}

impl Fingerprint {
    /// Include the next consecutive bytes.
    ///
    /// # Errors
    /// The cumulative byte count is out of range.
    pub fn update(&mut self, bytes: &[u8]) -> io::Result<()> {
        let size = u64::try_from(bytes.len()).map_err(io::Error::other)?;
        self.bytes = self
            .bytes
            .checked_add(size)
            .ok_or_else(|| io::Error::other("capture size overflow"))?;
        self.hash.update(bytes);
        Ok(())
    }

    /// Read a capture in bounded chunks.
    ///
    /// # Errors
    /// The reader fails or the cumulative byte count overflows.
    pub fn read(mut input: impl Read) -> io::Result<Self> {
        let mut fingerprint = Self::default();
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => return Ok(fingerprint),
                Ok(read) => fingerprint.update(&buffer[..read])?,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error),
            }
        }
    }

    /// Return the byte count and lowercase hexadecimal SHA-256.
    #[must_use]
    pub fn finish(self) -> (u64, String) {
        (self.bytes, hex::encode(self.hash.finalize()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunked_export_matches_buffered_selection_across_unicode_and_section_boundaries() {
        let payload = format!("-- TABLE: person\nDEFINE TABLE person;\n-- TABLE: draft\nDEFINE TABLE draft;\n-- TABLE DATA: draft\nCREATE draft:1 CONTENT {{text:'ignored'}};\n-- TABLE DATA: person\nCREATE person:1 CONTENT {{text:'{}é𐐀'}};\n-- END", "x".repeat(16_384));
        let expected = format!("-- TABLE: person\nDEFINE TABLE person;\n-- TABLE DATA: person\nCREATE person:1 CONTENT {{text:'{}é𐐀'}};\n-- END", "x".repeat(16_384)).into_bytes();
        for size in [1, 2, 3, 7, 127, 1024] {
            let mut filter = ExportFilter::new(["draft".to_owned()]);
            let mut actual = Vec::new();
            for chunk in payload.as_bytes().chunks(size) {
                filter
                    .feed(chunk, &mut |bytes| actual.extend_from_slice(bytes))
                    .unwrap();
                assert!(filter.prefix.capacity() <= 256);
            }
            filter
                .finish(&mut |bytes| actual.extend_from_slice(bytes))
                .unwrap();
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn banner_limit_fails_closed_while_large_payload_rows_stream() {
        let mut filter = ExportFilter::new(["draft".to_owned()]);
        assert!(filter
            .feed(format!("-- {}", "x".repeat(300)).as_bytes(), &mut |_| {})
            .is_err());
        let mut filter = ExportFilter::new(["draft".to_owned()]);
        let mut bytes = 0;
        filter
            .feed(&vec![b'x'; 1024 * 1024], &mut |chunk| bytes += chunk.len())
            .unwrap();
        filter.finish(&mut |chunk| bytes += chunk.len()).unwrap();
        assert_eq!(bytes, 1024 * 1024);
    }
    #[test]
    fn dash_banners_and_partial_final_headers_preserve_section_semantics() {
        let input = b"-- -----\n-- TABLE: draft\n-- -----\nCREATE draft:1;\n-- -----\n-- TABLE: person\n-- -----\nCREATE person:1;\n--";
        let expected = b"-- -----\n-- TABLE: person\n-- -----\nCREATE person:1;\n--";
        let mut filter = ExportFilter::new(["draft".to_owned()]);
        let mut actual = Vec::new();
        for chunk in input.chunks(2) {
            filter
                .feed(chunk, &mut |bytes| actual.extend_from_slice(bytes))
                .unwrap();
        }
        filter
            .finish(&mut |bytes| actual.extend_from_slice(bytes))
            .unwrap();
        assert_eq!(actual, expected);
        let mut invalid = ExportFilter::new([]);
        assert!(invalid.feed(b"-- \xff\n", &mut |_| {}).is_err());
    }
}
