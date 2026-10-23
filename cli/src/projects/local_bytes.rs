//! The offline half of `navigator project gate --check --deep`.
//!
//! A pointer's `current_version.sha256` and `size_bytes` describe the bytes
//! filed on the Project. The bytes themselves are Git-ignored files under
//! `documents/`, mirroring the pointer (`blobs/memo.md.yaml` names
//! `documents/memo.md`). This pass
//! hashes those local bytes and compares them with the pointer, so an edited or
//! corrupted file is caught without a request to the deployment.
//!
//! Each pointer lands in exactly one [`Status`]. Only [`Status::Modified`]
//! fails the gate: a checkout that never downloaded a document's bytes is
//! ordinary, so [`Status::Missing`] is reported and passes.

use std::path::Path;

use anyhow::{Context, Result};

use crate::document_read::{discover_pointers, slug_from_pointer};
use crate::document_sync::{read_pointer, slash_path};

/// What the local bytes say about one pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    /// The bytes hash and measure exactly as the pointer records.
    Match,
    /// The bytes exist and differ from the pointer; `reason` names how.
    Modified { reason: String },
    /// No file sits beside the pointer.
    Missing,
}

/// One pointer and what its local bytes say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Finding {
    /// The pointer path relative to the checkout root, with `/` separators.
    pub(crate) pointer: String,
    pub(crate) status: Status,
}

/// Every Blob under `dir/blobs`, each compared with its bytes under `dir/documents`.
///
/// A pointer that cannot be parsed is an error of the run, not a finding: the
/// content gate owns pointer syntax.
pub(crate) fn verify(dir: &Path) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();
    for relative in discover_pointers(dir)? {
        let absolute = dir.join(&relative);
        let Some(pointer) = read_pointer(&absolute)? else {
            continue;
        };
        let slug = slug_from_pointer(dir, &relative)?;
        // Bytes `project sync` has not yet moved into their matter folder
        // still sit at the slug itself (LAW-189).
        let laid_out = crate::document_layout::document_path(dir, Path::new(&slug), &pointer);
        let legacy = crate::document_sync::document_path_for_slug(dir, Path::new(&slug));
        let bytes_path = if !laid_out.exists() && legacy.is_file() {
            legacy
        } else {
            laid_out
        };
        let version = &pointer.current_version;
        let status = match std::fs::read(&bytes_path) {
            Ok(bytes) => compare(&bytes, &version.sha256, version.size_bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Status::Missing,
            Err(error) => {
                return Err(error).with_context(|| format!("read {}", bytes_path.display()))
            }
        };
        findings.push(Finding {
            pointer: slash_path(&relative)?,
            status,
        });
    }
    Ok(findings)
}

fn compare(bytes: &[u8], sha256: &str, size_bytes: i64) -> Status {
    let actual = store::documents::sha256_hex(bytes);
    let sizes_agree = i64::try_from(bytes.len()).is_ok_and(|len| len == size_bytes);
    if actual.eq_ignore_ascii_case(sha256) && sizes_agree {
        return Status::Match;
    }
    let reason = if actual.eq_ignore_ascii_case(sha256) {
        format!(
            "local size {} does not match the pointer's size_bytes {size_bytes}",
            bytes.len()
        )
    } else {
        format!("local bytes hash to {actual}, not the pointer's sha256 {sha256}")
    };
    Status::Modified { reason }
}

/// Counts for the one-line summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Tally {
    pub(crate) matched: usize,
    pub(crate) modified: usize,
    pub(crate) missing: usize,
}

#[must_use]
pub(crate) fn tally(findings: &[Finding]) -> Tally {
    findings
        .iter()
        .fold(Tally::default(), |mut tally, finding| {
            match finding.status {
                Status::Match => tally.matched += 1,
                Status::Modified { .. } => tally.modified += 1,
                Status::Missing => tally.missing += 1,
            }
            tally
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const BODY: &str = "Engagement memo, synthetic fixture.\n";

    fn pointer_yaml(bytes: &[u8]) -> String {
        format!(
            "kind: filing\nvisibility: internal\ncurrent_version:\n  version: 1\n  \
             asset_id: {}\n  created_at: \"2026-09-05T12:00:00Z\"\n  sha256: {}\n  \
             size_bytes: {}\n",
            Uuid::now_v7(),
            store::documents::sha256_hex(bytes),
            bytes.len()
        )
    }

    /// A checkout with `documents/memo.md` and its Blob, plus a Blob for
    /// `documents/absent.md` whose bytes were never downloaded.
    fn checkout() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let documents = dir.path().join("documents");
        let blobs = dir.path().join("blobs");
        std::fs::create_dir_all(&documents).unwrap();
        std::fs::create_dir_all(&blobs).unwrap();
        std::fs::write(documents.join("memo.md"), BODY).unwrap();
        std::fs::write(blobs.join("memo.md.yaml"), pointer_yaml(BODY.as_bytes())).unwrap();
        std::fs::write(blobs.join("absent.md.yaml"), pointer_yaml(b"never here")).unwrap();
        dir
    }

    fn status_of<'a>(findings: &'a [Finding], pointer: &str) -> &'a Status {
        &findings
            .iter()
            .find(|finding| finding.pointer == pointer)
            .unwrap()
            .status
    }

    #[test]
    fn a_clean_checkout_matches_and_reports_missing_bytes_separately() {
        let dir = checkout();
        let findings = verify(dir.path()).unwrap();
        assert_eq!(status_of(&findings, "blobs/memo.md.yaml"), &Status::Match);
        assert_eq!(
            status_of(&findings, "blobs/absent.md.yaml"),
            &Status::Missing
        );
        assert_eq!(
            tally(&findings),
            Tally {
                matched: 1,
                modified: 0,
                missing: 1
            }
        );
    }

    #[test]
    fn an_appended_line_is_modified() {
        let dir = checkout();
        let path = dir.path().join("documents/memo.md");
        std::fs::write(&path, format!("{BODY}tamper\n")).unwrap();
        let findings = verify(dir.path()).unwrap();
        assert!(matches!(
            status_of(&findings, "blobs/memo.md.yaml"),
            Status::Modified { reason } if reason.contains("hash")
        ));
        assert_eq!(tally(&findings).modified, 1);
    }

    #[test]
    fn a_size_mismatch_with_matching_hash_is_modified() {
        let dir = checkout();
        let pointer = dir.path().join("blobs/memo.md.yaml");
        let yaml = std::fs::read_to_string(&pointer).unwrap();
        let wrong = yaml.replace(
            &format!("size_bytes: {}", BODY.len()),
            &format!("size_bytes: {}", BODY.len() + 1),
        );
        std::fs::write(&pointer, wrong).unwrap();
        let findings = verify(dir.path()).unwrap();
        assert!(matches!(
            status_of(&findings, "blobs/memo.md.yaml"),
            Status::Modified { reason } if reason.contains("size")
        ));
    }

    #[test]
    fn a_checkout_with_no_documents_directory_has_no_findings() {
        let dir = tempfile::tempdir().unwrap();
        assert!(verify(dir.path()).unwrap().is_empty());
    }
}
