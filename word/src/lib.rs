//! Rust-owned boundary for Office Open XML Word packages.
//!
//! Markdown notations render to `.docx` here. Reading a package extracts the
//! main story's visible text after the safety preflight. Rust owns the
//! immutable source bytes, the typed document model, and the canonical
//! outline.

pub mod anchor;
pub mod import;
pub mod matter;
mod model;
pub mod notation;
pub mod outline;
pub mod paper;
mod preflight;
pub mod provenance;
pub mod redline;
mod render;
pub mod service;

pub use import::{
    apply_import, body_offset, diff_report, dry_run_import, plan_import, ImportError, ImportPlan,
};
pub use model::{
    Block, BreakKind, Comment, Diagnostic, DiagnosticCode, DiagnosticSeverity, DocumentModel,
    FieldKind, Inline, NumberingDefinition, NumberingIdentity, PackageInventory, PackagePart,
    PackageRelationship, Paragraph, RevisionKind, RevisionNode, Story, StoryKind, StyleDefinition,
    Table, TableCell, TableRow,
};
pub use notation::{to_markdown, TrustedNotationMarkdown};
pub use outline::{
    BlockEditError, BlockManifestEntry, CanonicalBlock, CanonicalBlockKind, CanonicalDocument,
    CanonicalInline, CanonicalStory, ListIdentity, NumberingLevel, OutlineScheme, OutlineUnit,
    HARVARD_OUTLINE_PATTERN, MARKER_GROUPS, MAX_DEPTH,
};
pub use preflight::is_docx_filename;
pub use provenance::{
    read_provenance, template_hash, template_hash_of_path, verify_template_hash, Provenance,
    ProvenanceError, ProvenanceRecord, PROVENANCE_PART, PROVENANCE_RELATIONSHIP_TYPE,
};
pub use render::{
    render_notation, source_blocks, PleadingFrame, RenderError, RenderLetterhead, RenderOptions,
    SourceBlock, SourceBlockKind,
};

/// The model version carried on a parsed document. A lossy reading of a legal
/// document is worse than a visible refusal, so callers that need structure
/// the visible-text reader does not recover stop rather than guessing.
pub const PROTOCOL_VERSION: u16 = 1;

/// The main story's visible text. The filename classifies the `.docx` family
/// and never appears in the error. Insertions stored as `w:t` are kept;
/// deletions stored as `w:delText` are not.
pub fn package_text(filename: &str, bytes: &[u8]) -> Result<String, WordError> {
    preflight::validate_filename(filename)?;
    preflight::validate_zip(bytes)?;
    import::accepted_text(bytes).ok_or(WordError::CorruptPackage)
}

/// Errors at the Rust-owned document boundary. These variants intentionally
/// carry no filename, extracted text, package URI, or party data.
#[derive(Debug, thiserror::Error)]
pub enum WordError {
    #[error("unsupported Word package format")]
    UnsupportedFormat,
    #[error("legacy Word package rejected")]
    LegacyPackage,
    #[error("macro-enabled Word package rejected")]
    MacroEnabledPackage,
    #[error("encrypted Word package rejected")]
    EncryptedPackage,
    #[error("Word package is corrupt")]
    CorruptPackage,
    #[error("Word package path escapes its container")]
    EscapingPackage,
    #[error("Word package ZIP entry count {actual} exceeds maximum {maximum}")]
    ZipEntryCountExceeded { actual: usize, maximum: usize },
    #[error(
        "Word package ZIP entry uncompressed size {actual} bytes exceeds maximum {maximum} bytes"
    )]
    ZipEntryUncompressedSizeExceeded { actual: u64, maximum: u64 },
    #[error(
        "Word package total ZIP uncompressed size {actual} bytes exceeds maximum {maximum} bytes"
    )]
    ZipTotalUncompressedSizeExceeded { actual: u64, maximum: u64 },
    #[error("Word package ZIP entry inflated size {actual} bytes exceeds maximum {maximum} bytes")]
    ZipEntryInflatedSizeExceeded { actual: u64, maximum: u64 },
}

#[cfg(test)]
mod tests {
    use super::WordError;

    #[test]
    fn package_text_keeps_insertions_and_drops_deletions() {
        let document = concat!(
            r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">"#,
            "<w:body>",
            "<w:p><w:r><w:t>kept</w:t></w:r>",
            r#"<w:ins w:id="1"><w:r><w:t> added</w:t></w:r></w:ins>"#,
            r#"<w:del w:id="2"><w:r><w:delText> removed</w:delText></w:r></w:del>"#,
            "</w:p>",
            "<w:p><w:r><w:t>next</w:t></w:r></w:p>",
            "</w:body></w:document>",
        );
        let text = super::package_text("synthetic.docx", &docx_with_document(document))
            .expect("a readable package");
        assert_eq!(text, "kept added\nnext");
        assert!(!text.contains("removed"));
        let error = super::package_text("legacy.doc", &docx_with_document(document))
            .expect_err("a legacy name is refused before the body is read");
        assert!(matches!(error, WordError::LegacyPackage));
        assert!(!error.to_string().contains("synthetic"));
    }

    fn docx_with_document(document: &str) -> Vec<u8> {
        use std::io::Write as _;
        let mut bytes = std::io::Cursor::new(Vec::new());
        let mut zip = zip::ZipWriter::new(&mut bytes);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("[Content_Types].xml", options).unwrap();
        zip.write_all(b"<Types/>").unwrap();
        zip.start_file("word/document.xml", options).unwrap();
        zip.write_all(document.as_bytes()).unwrap();
        zip.finish().unwrap();
        bytes.into_inner()
    }
}
