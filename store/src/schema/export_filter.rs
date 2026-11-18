//! Bounded streaming selection of canonical operational export sections.

use std::collections::BTreeSet;

pub(super) struct Sections {
    derived: BTreeSet<String>,
    skipping: bool,
}

impl Sections {
    pub(super) fn new(derived: impl IntoIterator<Item = String>) -> Self {
        Self {
            derived: derived.into_iter().collect(),
            skipping: false,
        }
    }

    pub(super) fn retain(&mut self, line: &str) -> bool {
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
pub(crate) struct FilterError;

/// Stream payload rows directly; retain only one bounded section banner.
pub(crate) struct ExportFilter {
    sections: Sections,
    prefix: Vec<u8>,
    content: bool,
}

impl ExportFilter {
    /// Use the same derived-table set as the canonical buffered export.
    pub(crate) fn new(derived: impl IntoIterator<Item = String>) -> Self {
        Self {
            sections: Sections::new(derived),
            prefix: Vec::with_capacity(256),
            content: false,
        }
    }

    /// Feed one transport chunk without retaining its payload rows.
    pub(crate) fn feed(
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

    /// Finish a final banner without a newline; content was already streamed.
    pub(crate) fn finish(&mut self, emit: &mut impl FnMut(&[u8])) -> Result<(), FilterError> {
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
