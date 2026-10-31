//! Export provenance: the record a Navigator Word render embeds about the
//! template it came from, and the checks `notation import` runs against it.
//!
//! A `.docx` a lawyer edits is only importable when Navigator can prove which
//! template body produced it. The render writes one small XML part,
//! [`PROVENANCE_PART`], carrying the template's path as exported and the
//! template file's SHA-256 at export time. Import refuses a package without
//! the part (it is not a Navigator export) and refuses a package whose hash
//! no longer matches the template on disk (the template moved on after the
//! draft was cut, so block alignment cannot be trusted).
//!
//! The part is additive: Word preserves package parts and relationships it
//! does not understand, so the record survives a lawyer's round trip through
//! Word itself.

use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};
use zip::ZipArchive;

/// The package part carrying the export provenance record.
pub const PROVENANCE_PART: &str = "docProps/navigator.xml";

/// The relationship type that binds [`PROVENANCE_PART`] into the package.
pub const PROVENANCE_RELATIONSHIP_TYPE: &str = "https://neonlaw.com/navigator/xml/2026/provenance";

/// The provenance a Word render is asked to embed, taken from the template
/// at export time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance<'a> {
    /// The template's file name as the exporting command received it — a
    /// name, not a full path, so the same template renders byte-identically
    /// from any directory. Import verifies the template by hash against the
    /// path the command was given; the name is advisory context.
    pub template_path: &'a str,
    /// SHA-256 of the template file's bytes at export time, hex-encoded.
    pub template_hash: &'a str,
}

impl Provenance<'_> {
    /// The XML part. One self-contained element; attributes are escaped for
    /// XML the same way the renderer escapes every other part.
    #[must_use]
    pub fn to_xml(&self) -> String {
        format!(
            concat!(
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>",
                "<navigator:provenance xmlns:navigator=\"{}\" template-path=\"{}\" ",
                "template-hash=\"{}\"/>"
            ),
            PROVENANCE_RELATIONSHIP_TYPE,
            xml_escape(self.template_path),
            xml_escape(self.template_hash),
        )
    }
}

/// The provenance read back out of an edited package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenanceRecord {
    pub template_path: String,
    pub template_hash: String,
}

/// Why a package's provenance could not be trusted for an import.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProvenanceError {
    #[error("the Word package carries no Navigator export provenance; re-export the draft with `navigator notation word`")]
    Absent,
    #[error("the Word package could not be read")]
    Unreadable,
    #[error("the Navigator provenance part is malformed")]
    Malformed,
    #[error("the Navigator provenance part is missing its `{0}` attribute")]
    MissingAttribute(&'static str),
    #[error(
        "the template changed since this draft was exported (export hash {exported}, current {current}); \
         re-export a fresh draft before importing"
    )]
    HashMismatch { exported: String, current: String },
}

/// SHA-256 of a template file's bytes, hex-encoded — the hash the render
/// embeds and import verifies.
#[must_use]
pub fn template_hash(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

/// Hash the template at `path` for export. I/O lives here so both the render
/// path and tests share one definition of "the template's content hash".
///
/// # Errors
/// The read error when the template cannot be read.
pub fn template_hash_of_path(path: &Path) -> std::io::Result<String> {
    Ok(template_hash(&std::fs::read(path)?))
}

/// Read the provenance record out of a Word package.
///
/// # Errors
/// [`ProvenanceError::Absent`] when the package carries no provenance part,
/// [`ProvenanceError::Malformed`] or [`ProvenanceError::MissingAttribute`]
/// when the part is not the record the renderer writes.
pub fn read_provenance(docx: &[u8]) -> Result<ProvenanceRecord, ProvenanceError> {
    let mut archive =
        ZipArchive::new(std::io::Cursor::new(docx)).map_err(|_| ProvenanceError::Unreadable)?;
    let mut part = archive
        .by_name(PROVENANCE_PART)
        .map_err(|_| ProvenanceError::Absent)?;
    let mut xml = String::new();
    part.read_to_string(&mut xml)
        .map_err(|_| ProvenanceError::Malformed)?;

    let path = attribute(&xml, "template-path")
        .ok_or(ProvenanceError::MissingAttribute("template-path"))?;
    let hash = attribute(&xml, "template-hash")
        .ok_or(ProvenanceError::MissingAttribute("template-hash"))?;
    Ok(ProvenanceRecord {
        template_path: path,
        template_hash: hash,
    })
}

/// Verify a package's export provenance against the template currently on
/// disk. A mismatch means the template moved on after the draft was cut.
///
/// # Errors
/// [`ProvenanceError::HashMismatch`] when the current template's hash differs
/// from the recorded export hash.
pub fn verify_template_hash(
    record: &ProvenanceRecord,
    current_template_bytes: &[u8],
) -> Result<(), ProvenanceError> {
    let current = template_hash(current_template_bytes);
    if current == record.template_hash {
        Ok(())
    } else {
        Err(ProvenanceError::HashMismatch {
            exported: record.template_hash.clone(),
            current,
        })
    }
}

/// One attribute of the provenance element. The renderer writes this part
/// itself, so a boundary scan with XML-entity unescaping is the whole grammar.
fn attribute(xml: &str, name: &str) -> Option<String> {
    let marker = format!("{name}=\"");
    let start = xml.find(&marker)? + marker.len();
    let rest = &xml[start..];
    let end = rest.find('"')?;
    Some(xml_unescape(&rest[..end]))
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::{
        read_provenance, template_hash, verify_template_hash, Provenance, ProvenanceError,
        ProvenanceRecord, PROVENANCE_PART,
    };
    use crate::render::render_notation;
    use crate::render::{RenderLetterhead, RenderOptions};

    const BODY: &str =
        "# Draft\n\nFirst paragraph with {{answer_code}}.\n\n## Section\n\nSecond paragraph.\n";

    #[test]
    fn a_render_with_provenance_reads_back_to_the_same_record() {
        let bytes = render_notation(
            BODY,
            None,
            &RenderOptions {
                provenance: Some(Provenance {
                    template_path: "templates/nevada/llc.md",
                    template_hash: "abc123",
                }),
                ..RenderOptions::default()
            },
        )
        .unwrap();
        let record = read_provenance(&bytes).expect("provenance part present");
        assert_eq!(
            record,
            ProvenanceRecord {
                template_path: "templates/nevada/llc.md".into(),
                template_hash: "abc123".into(),
            }
        );
    }

    #[test]
    fn a_render_without_provenance_carries_no_part() {
        let bytes = render_notation(BODY, None, &RenderOptions::default()).unwrap();
        assert!(matches!(
            read_provenance(&bytes),
            Err(ProvenanceError::Absent)
        ));
    }

    #[test]
    fn provenance_attribute_escaping_survives_a_round_trip() {
        let bytes = render_notation(
            BODY,
            None,
            &RenderOptions {
                provenance: Some(Provenance {
                    template_path: "templates/a&b\"c.md",
                    template_hash: "abc123",
                }),
                ..RenderOptions::default()
            },
        )
        .unwrap();
        let record = read_provenance(&bytes).unwrap();
        assert_eq!(record.template_path, "templates/a&b\"c.md");
    }

    #[test]
    fn a_foreign_package_has_no_provenance() {
        // A minimal ZIP that is not a Word package at all.
        use std::io::Write as _;
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive
            .start_file("readme.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"not a docx").unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        assert!(matches!(
            read_provenance(&bytes),
            Err(ProvenanceError::Absent)
        ));
    }

    #[test]
    fn a_malformed_provenance_part_is_refused() {
        use std::io::Write as _;
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        archive
            .start_file(PROVENANCE_PART, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"<no attributes here>").unwrap();
        let bytes = archive.finish().unwrap().into_inner();
        assert!(matches!(
            read_provenance(&bytes),
            Err(ProvenanceError::MissingAttribute("template-path"))
        ));
    }

    #[test]
    fn the_hash_check_accepts_the_same_template_and_refuses_a_changed_one() {
        let record = ProvenanceRecord {
            template_path: "templates/x.md".into(),
            template_hash: template_hash(b"template body"),
        };
        assert!(verify_template_hash(&record, b"template body").is_ok());
        let error = verify_template_hash(&record, b"edited template body").unwrap_err();
        assert!(matches!(error, ProvenanceError::HashMismatch { .. }));
        assert!(error.to_string().contains("re-export"));
    }

    #[test]
    fn a_letterhead_render_keeps_the_provenance_part() {
        let letterhead = RenderLetterhead {
            name: "Neon Law",
            phone: "702",
            email: "hello@neonlaw.com",
            web: "neonlaw.com",
            logo_png: &[],
        };
        let bytes = render_notation(
            BODY,
            Some(&letterhead),
            &RenderOptions {
                provenance: Some(Provenance {
                    template_path: "t.md",
                    template_hash: "abc123",
                }),
                ..RenderOptions::default()
            },
        )
        .unwrap();
        assert!(read_provenance(&bytes).is_ok());
    }
}
