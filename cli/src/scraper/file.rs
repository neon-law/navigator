//! DKT filing for scraped documents: a staged, hashed document becomes a
//! Navigator docket filing.
//!
//! Naming is Navigator's own, never the scraper's: the flat
//! `DKT NNNN[-att] - <title>.<ext>` name `store::cases` prescribes, the
//! bytes under `documents/Docket Filings/`, and the source-only YAML
//! pointer at `blobs/<name>.yaml` — identical structure for every case
//! and every source. The naming module is reused, not forked; this file
//! adds only what Navigator's court filings never needed: choosing among
//! escalation names when two distinct documents map to one DKT name
//! (`DKT NNNN-att`, `DKT NNNN-att2`, ...), and idempotence — re-filing a
//! document that is already on disk with the same hash files nothing and
//! reports it as already filed.
//!
//! Storage sits behind the [`FilingStore`] trait: the pure functions here
//! decide *what* to file and *under which name*; the store decides *how*.
//! [`ProjectFilingStore`] writes a Project checkout directly and backs the
//! [`DktFilingIntake`] implementation of the run's intake seam. The
//! omnichannel inbox intake — triage being the only path into a matter
//! folder — is its own integration behind the same seam and stays
//! pluggable with this one.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::scraper::download::hex_digest;
use crate::scraper::intake::{IntakeError, IntakeReceipt, InboxIntake, ScrapedDocument};

/// How many collision escalations one filing may walk before it stops:
/// `-att`, then `-att2` through `-att32`. A bound keeps a pathological
/// folder from turning one enqueue into an unbounded walk; a docket that
/// exhausts it needs a human, not a bigger number.
const MAX_ESCALATIONS: u32 = 32;

/// Why a document could not be filed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum FilingError {
    #[error(transparent)]
    Naming(#[from] store::cases::FilingNameError),
    #[error(
        "every filing name through `att{escalations}` is taken by other documents; \
         a docket this crowded needs a human look"
    )]
    Exhausted { escalations: u32 },
    #[error("the document has no bytes; an empty document is never filed")]
    Empty,
    #[error("the filing store failed")]
    Storage(#[from] StorageError),
}

/// Why a filing store could not complete an operation.
#[derive(Debug, thiserror::Error)]
pub(crate) enum StorageError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("{name}: {detail}")]
    Pointer { name: String, detail: String },
}

/// A fetched document with the metadata an adapter read off the docket,
/// ready to file. The `sha256` is derived from the bytes here — never
/// trusted from the wire — so the pointer the store commits and the
/// idempotence check both describe exactly what lands on disk.
#[derive(Clone)]
pub(crate) struct FilingRequest {
    /// The entry number as the caller presents it: `4`, `0004`, a court
    /// attachment suffix as printed (`29-1`), or a `DKT 4` spelling from a
    /// listing; normalized to the prescribed `DKT NNNN` form at naming.
    pub(crate) entry_number: String,
    /// An attachment the court prints no number for: the primary name is
    /// the `DKT NNNN-att` form instead of the plain one.
    pub(crate) unnumbered_attachment: bool,
    /// The entry's title, verbatim as the docket prints it.
    pub(crate) title: String,
    pub(crate) extension: String,
    pub(crate) bytes: Vec<u8>,
}

impl FilingRequest {
    /// Wrap a fetched document for filing.
    ///
    /// # Errors
    ///
    /// When the document has no bytes: an empty document is a scrape bug,
    /// and a pointer's `size_bytes` must be positive anyway.
    pub(crate) fn new(
        entry_number: impl Into<String>,
        unnumbered_attachment: bool,
        title: impl Into<String>,
        extension: impl Into<String>,
        bytes: Vec<u8>,
    ) -> Result<Self, FilingError> {
        if bytes.is_empty() {
            return Err(FilingError::Empty);
        }
        Ok(Self {
            entry_number: entry_number.into(),
            unnumbered_attachment,
            title: title.into(),
            extension: extension.into(),
            bytes,
        })
    }

    /// Wrap a scraped document staged by the download pipeline.
    ///
    /// # Errors
    ///
    /// When the document has no bytes.
    pub(crate) fn from_scraped(document: &ScrapedDocument) -> Result<Self, FilingError> {
        Self::new(
            document.entry_number.clone(),
            false,
            document.title.clone(),
            document.extension.clone(),
            document.bytes.clone(),
        )
    }

    /// The lowercase hex SHA-256 of the document's bytes.
    pub(crate) fn sha256(&self) -> String {
        hex_digest(&Sha256::digest(&self.bytes))
    }

    /// The primary filing name: the plain `DKT NNNN` form, or the
    /// `DKT NNNN-att` form for an unnumbered attachment.
    ///
    /// # Errors
    ///
    /// When the court's entry number cannot be normalized to the
    /// prescribed `DKT NNNN[-att]` form.
    pub(crate) fn primary_name(&self) -> Result<String, store::cases::FilingNameError> {
        self.name_at_escalation(if self.unnumbered_attachment { 1 } else { 0 })
    }

    /// The filing name at `escalation`: `0` the plain `DKT NNNN` form, `1`
    /// the `DKT NNNN-att` form, `2` and beyond `DKT NNNN-att2`, ...
    ///
    /// # Errors
    ///
    /// When the court's entry number cannot be normalized.
    pub(crate) fn name_at_escalation(
        &self,
        escalation: u32,
    ) -> Result<String, store::cases::FilingNameError> {
        store::cases::escalated_filing_name(
            normalized_entry(&self.entry_number),
            escalation,
            &self.title,
            &self.extension,
        )
    }
}

/// The entry number `store::cases` consumes: a leading `DKT ` (the way a
/// listing prints the number) and any surrounding whitespace are dropped.
/// Nothing else about the court's number changes.
fn normalized_entry(entry_number: &str) -> &str {
    let trimmed = entry_number.trim();
    let bytes = trimmed.as_bytes();
    let prefixed = bytes.len() >= 4
        && bytes[..3].eq_ignore_ascii_case(b"DKT")
        && bytes[3] == b' ';
    if prefixed {
        trimmed[4..].trim()
    } else {
        trimmed
    }
}

/// Where a filing landed in the Project checkout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FiledPaths {
    pub(crate) bytes: PathBuf,
    pub(crate) pointer: PathBuf,
}

/// The seam between the pure filing decision and wherever the documents
/// and pointers live. Production writes a Project checkout
/// ([`ProjectFilingStore`]); tests record in memory.
pub(crate) trait FilingStore: Send + Sync {
    /// The SHA-256 of the bytes currently filed under `name`, when any are.
    ///
    /// # Errors
    ///
    /// When reading the filed bytes fails for a reason other than absence.
    fn sha256_of(&self, name: &str) -> Result<Option<String>, StorageError>;

    /// Write `request`'s bytes and pointer under `name`. The caller has
    /// established that `name` is free, so an implementation may assume it
    /// is writing a document that is not already there.
    ///
    /// # Errors
    ///
    /// When either write fails.
    fn file(&self, name: &str, request: &FilingRequest) -> Result<FiledPaths, StorageError>;
}

/// What one filing did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum FilingOutcome {
    /// The bytes and pointer were written under `name`.
    Filed {
        name: String,
        paths: FiledPaths,
    },
    /// The same document — same hash — was already filed under `name`;
    /// nothing was written.
    AlreadyFiled { name: String },
}

/// File one document: walk the escalation names from the primary form, and
/// stop at the first one that is free; a name already holding the same
/// document means it was filed before, and filing nothing again is the
/// idempotent answer. A name holding a *different* document is a collision,
/// and the next escalation resolves it.
///
/// # Errors
///
/// When the entry number cannot be normalized, the store fails, or every
/// escalation name through [`MAX_ESCALATIONS`] is taken.
pub(crate) fn resolve_filing(
    store: &dyn FilingStore,
    request: &FilingRequest,
) -> Result<FilingOutcome, FilingError> {
    // Every comparison below derives from the same bytes, hashed once.
    let sha256 = request.sha256();
    let first = u32::from(request.unnumbered_attachment);
    for escalation in first..=first.saturating_add(MAX_ESCALATIONS) {
        let name = request.name_at_escalation(escalation)?;
        match store.sha256_of(&name)? {
            Some(existing) if existing == sha256 => {
                return Ok(FilingOutcome::AlreadyFiled { name });
            }
            Some(_) => continue,
            None => {
                let paths = store.file(&name, request)?;
                return Ok(FilingOutcome::Filed { name, paths });
            }
        }
    }
    Err(FilingError::Exhausted {
        escalations: MAX_ESCALATIONS,
    })
}

/// Files into a Project checkout: bytes under
/// `documents/Docket Filings/<name>` (git-ignored working material), the
/// YAML pointer at `blobs/<name>.yaml` (the committed record). The pointer
/// is the source-only blob `store::document_pointers::DocumentPointer`
/// prescribes — metadata and an `assets` row id, never bytes — at version
/// 1, kind `pleading` (the Docket Filings default), internal visibility,
/// with the same hash and size as the bytes written beside it.
pub(crate) struct ProjectFilingStore {
    root: PathBuf,
}

impl ProjectFilingStore {
    /// A store filing into the Project checkout at `root`.
    pub(crate) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Where the bytes for `name` sit below the checkout: flat in the
    /// `Docket Filings` matter folder, whatever the entry's kind (LAW-189).
    fn bytes_path(&self, name: &str) -> PathBuf {
        self.root
            .join(crate::document_sync::DOCUMENT_DIRECTORY)
            .join(crate::document_layout::bytes_relative(
                Path::new(name),
                store::document_pointers::MatterFolder::DocketFilings,
            ))
    }
}

impl FilingStore for ProjectFilingStore {
    fn sha256_of(&self, name: &str) -> Result<Option<String>, StorageError> {
        let path = self.bytes_path(name);
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(hex_digest(&Sha256::digest(&bytes)))),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(StorageError::Io { path, source }),
        }
    }

    fn file(&self, name: &str, request: &FilingRequest) -> Result<FiledPaths, StorageError> {
        let bytes_path = self.bytes_path(name);
        // The same safety checks every managed document write runs: the
        // path stays inside the checkout's managed tree, and the bytes
        // stay git-ignored.
        crate::document_sync::ensure_managed_copy_target(&self.root, &bytes_path)
            .map_err(|error| StorageError::Pointer {
                name: name.to_owned(),
                detail: format!("{error:#}"),
            })?;
        crate::scraper::fs::atomic_write(&bytes_path, &request.bytes).map_err(|source| {
            StorageError::Io {
                path: bytes_path.clone(),
                source,
            }
        })?;
        let pointer = pointer_for(
            request,
            uuid::Uuid::now_v7(),
            chrono::Utc::now().to_rfc3339(),
        );
        let pointer_path =
            crate::document_sync::write_uploaded_pointer(&self.root, name, &pointer).map_err(
                |error| StorageError::Pointer {
                    name: name.to_owned(),
                    detail: format!("{error:#}"),
                },
            )?;
        Ok(FiledPaths {
            bytes: bytes_path,
            pointer: pointer_path,
        })
    }
}

/// The source-only pointer a filed document commits: version 1, kind
/// `pleading` (which already maps to Docket Filings, so the pointer names
/// no `folder:` of its own — the same receipt `project sync` records),
/// internal visibility, and the immutable facts of the bytes just written.
fn pointer_for(
    request: &FilingRequest,
    asset_id: uuid::Uuid,
    created_at: String,
) -> store::document_pointers::DocumentPointer {
    store::document_pointers::DocumentPointer {
        kind: store::document_pointers::MatterFolder::DocketFilings.default_kind().to_owned(),
        visibility: "internal".to_owned(),
        folder: None,
        current_version: store::document_pointers::PointerVersion {
            version: 1,
            asset_id,
            created_at,
            sha256: request.sha256(),
            size_bytes: request.bytes.len().cast_signed(),
        },
        previous_version: None,
        authority_id: None,
        docusign_envelope_id: None,
        signing: None,
        notarization: Vec::new(),
        service: Vec::new(),
        evidence: None,
    }
}

/// The intake-seam implementation that files docket documents straight
/// into a Project checkout under their DKT names — the direct counterpart
/// to the omnichannel inbox intake, which is its own integration behind
/// the same [`InboxIntake`] seam. A run picks one; neither is a fork.
pub(crate) struct DktFilingIntake<'a> {
    store: &'a dyn FilingStore,
}

impl<'a> DktFilingIntake<'a> {
    /// An intake filing through `store`.
    pub(crate) fn new(store: &'a dyn FilingStore) -> Self {
        Self { store }
    }
}

#[async_trait::async_trait]
impl InboxIntake for DktFilingIntake<'_> {
    async fn enqueue(&self, document: &ScrapedDocument) -> Result<IntakeReceipt, IntakeError> {
        let request =
            FilingRequest::from_scraped(document).map_err(|error| IntakeError::Rejected {
                detail: error.to_string(),
            })?;
        match resolve_filing(self.store, &request) {
            Ok(FilingOutcome::Filed { .. }) => Ok(IntakeReceipt {
                message_id: uuid::Uuid::now_v7(),
                deduped: false,
            }),
            // The document was already filed — the honest receipt is a
            // deduped one, the same shape a lost-ack retry gets from the
            // inbox.
            Ok(FilingOutcome::AlreadyFiled { .. }) => Ok(IntakeReceipt {
                message_id: uuid::Uuid::now_v7(),
                deduped: true,
            }),
            Err(FilingError::Storage(source)) => Err(IntakeError::Transient {
                detail: source.to_string(),
            }),
            Err(error) => Err(IntakeError::Rejected {
                detail: error.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use sha2::{Digest, Sha256};

    use super::{
        normalized_entry, pointer_for, resolve_filing, DktFilingIntake, FilingError,
        FilingOutcome, FilingRequest, FiledPaths, FilingStore, ProjectFilingStore, StorageError,
    };
    use crate::scraper::adapter::SiteKey;
    use crate::scraper::download::hex_digest;
    use crate::scraper::intake::{IntakeError, ScrapedDocument};

    const DOCUMENT_BYTES: &[u8] = b"%PDF-1.7 docket notice bytes";
    const OTHER_BYTES: &[u8] = b"%PDF-1.7 a different document";

    fn sha_of(bytes: &[u8]) -> String {
        hex_digest(&Sha256::digest(bytes))
    }

    fn request(entry: &str, title: &str) -> FilingRequest {
        FilingRequest::new(entry, false, title, "pdf", DOCUMENT_BYTES.to_vec())
            .expect("the document has bytes")
    }

    fn scraped(entry: &str, title: &str) -> ScrapedDocument {
        let site = SiteKey::new("la-superior").expect("valid key");
        let notice = crate::scraper::adapter::NoticeRef {
            entry_number: entry.to_owned(),
            title: title.to_owned(),
            url: "https://portal.example.com/doc".to_owned(),
            extension: "pdf".to_owned(),
        };
        ScrapedDocument::from_download(
            site,
            "0004",
            &notice,
            sha_of(DOCUMENT_BYTES),
            DOCUMENT_BYTES.to_vec(),
        )
        .expect("the entry number is normalizable")
    }

    /// An in-memory [`FilingStore`] double: files by name, so collision,
    /// escalation, and idempotence run with no filesystem at all.
    #[derive(Default)]
    struct MemoryStore {
        files: Mutex<BTreeMap<String, Vec<u8>>>,
        pointers: Mutex<BTreeMap<String, String>>,
        calls: std::sync::atomic::AtomicU32,
    }

    impl FilingStore for MemoryStore {
        fn sha256_of(&self, name: &str) -> Result<Option<String>, StorageError> {
            Ok(self
                .files
                .lock()
                .expect("uncontended")
                .get(name)
                .map(|bytes| hex_digest(&Sha256::digest(bytes))))
        }

        fn file(&self, name: &str, request: &FilingRequest) -> Result<FiledPaths, StorageError> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let pointer = pointer_for(
                request,
                uuid::Uuid::nil(),
                "1970-01-01T00:00:00+00:00".to_owned(),
            );
            self.files
                .lock()
                .expect("uncontended")
                .insert(name.to_owned(), request.bytes.clone());
            self.pointers
                .lock()
                .expect("uncontended")
                .insert(
                    name.to_owned(),
                    pointer.to_yaml().expect("the pointer serializes"),
                );
            Ok(FiledPaths {
                bytes: PathBuf::from(name),
                pointer: PathBuf::from(format!("{name}.yaml")),
            })
        }
    }

    // ---- Name derivation ------------------------------------------------

    #[test]
    fn entry_variants_normalize_to_one_dkt_name() {
        for entry in ["4", "0004", "DKT 4", "dkt 4", "  4  "] {
            let request = request(entry, "Notice of Hearing");
            assert_eq!(
                request.primary_name().expect("normalizable"),
                "DKT 0004 - Notice of Hearing.pdf",
                "entry `{entry}` normalizes to the prescribed form"
            );
        }
    }

    #[test]
    fn the_pure_normalizer_strips_the_listing_prefix() {
        assert_eq!(normalized_entry("4"), "4");
        assert_eq!(normalized_entry("DKT 4"), "4");
        assert_eq!(normalized_entry("dkt 4"), "4");
        assert_eq!(normalized_entry("  0004 "), "0004");
    }

    #[test]
    fn court_attachment_suffixes_keep_their_number() {
        let request = request("29-1", "Exhibit A");
        assert_eq!(
            request.primary_name().expect("normalizable"),
            "DKT 0029-1 - Exhibit A.pdf"
        );
    }

    #[test]
    fn unnumbered_attachments_file_under_att() {
        let request = FilingRequest::new("4", true, "Exhibit A", "pdf", DOCUMENT_BYTES.to_vec())
            .expect("the document has bytes");
        assert_eq!(
            request.primary_name().expect("normalizable"),
            "DKT 0004-att - Exhibit A.pdf"
        );
    }

    #[test]
    fn titles_are_sanitized_and_escalations_compose() {
        let request = request("4", "Motion re: A/B");
        assert_eq!(
            request.primary_name().expect("normalizable"),
            "DKT 0004 - Motion re- A-B.pdf"
        );
        let request = request("4", "Order");
        assert_eq!(
            request.name_at_escalation(2).expect("normalizable"),
            "DKT 0004-att2 - Order.pdf"
        );
    }

    #[test]
    fn a_nonnumeric_entry_number_is_rejected_with_the_store_error() {
        let request = request("x", "Order");
        assert!(matches!(
            request.primary_name(),
            Err(store::cases::FilingNameError(_))
        ));
    }

    #[test]
    fn an_empty_document_is_never_filed() {
        let outcome = FilingRequest::new("4", false, "Order", "pdf", Vec::new());
        assert!(matches!(outcome, Err(FilingError::Empty)));
    }

    // ---- Collision escalation and idempotence (in-memory) ---------------

    #[test]
    fn a_fresh_name_files_at_the_primary_escalation() {
        let store = MemoryStore::default();
        let outcome = resolve_filing(&store, &request("4", "Notice of Hearing")).expect("filed");
        assert_eq!(
            outcome,
            FilingOutcome::Filed {
                name: "DKT 0004 - Notice of Hearing.pdf".to_owned(),
                paths: FiledPaths {
                    bytes: PathBuf::from("DKT 0004 - Notice of Hearing.pdf"),
                    pointer: PathBuf::from("DKT 0004 - Notice of Hearing.pdf.yaml"),
                },
            }
        );
        assert_eq!(store.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn re_filing_the_same_document_is_idempotent() {
        let store = MemoryStore::default();
        resolve_filing(&store, &request("4", "Notice of Hearing")).expect("filed");
        let outcome =
            resolve_filing(&store, &request("4", "Notice of Hearing")).expect("already filed");
        assert_eq!(
            outcome,
            FilingOutcome::AlreadyFiled {
                name: "DKT 0004 - Notice of Hearing.pdf".to_owned(),
            }
        );
        assert_eq!(store.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn a_collision_escalates_to_att_then_att2() {
        let store = MemoryStore::default();
        resolve_filing(&store, &request("4", "Notice of Hearing")).expect("filed");
        let second = resolve_filing(&store, &request("4", "Amended Notice")).expect("filed");
        assert_eq!(
            second,
            FilingOutcome::Filed {
                name: "DKT 0004-att - Amended Notice.pdf".to_owned(),
                paths: FiledPaths {
                    bytes: PathBuf::from("DKT 0004-att - Amended Notice.pdf"),
                    pointer: PathBuf::from("DKT 0004-att - Amended Notice.pdf.yaml"),
                },
            }
        );
        let third = resolve_filing(&store, &request("4", "Second Amendment")).expect("filed");
        assert_eq!(
            third,
            FilingOutcome::Filed {
                name: "DKT 0004-att2 - Second Amendment.pdf".to_owned(),
                paths: FiledPaths {
                    bytes: PathBuf::from("DKT 0004-att2 - Second Amendment.pdf"),
                    pointer: PathBuf::from("DKT 0004-att2 - Second Amendment.pdf.yaml"),
                },
            }
        );
    }

    #[test]
    fn an_unnumbered_attachment_colliding_escalates_beyond_att() {
        let store = MemoryStore::default();
        let attachment = |title: &str| {
            FilingRequest::new("4", true, title, "pdf", DOCUMENT_BYTES.to_vec())
                .expect("the document has bytes")
        };
        resolve_filing(&store, &attachment("Exhibit A")).expect("filed");
        let second = resolve_filing(&store, &attachment("Exhibit B")).expect("filed");
        assert_eq!(
            second,
            FilingOutcome::Filed {
                name: "DKT 0004-att2 - Exhibit B.pdf".to_owned(),
                paths: FiledPaths {
                    bytes: PathBuf::from("DKT 0004-att2 - Exhibit B.pdf"),
                    pointer: PathBuf::from("DKT 0004-att2 - Exhibit B.pdf.yaml"),
                },
            }
        );
    }

    // ---- The real store: layout and pointer conventions (tempdir) -------

    #[test]
    fn the_project_store_files_bytes_and_pointer_in_the_prescribed_layout() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let store = ProjectFilingStore::new(root.clone());
        let outcome = resolve_filing(&store, &request("4", "Notice of Hearing")).expect("filed");
        let FilingOutcome::Filed { name, paths } = outcome else {
            panic!("a fresh name files, it does not dedupe");
        };
        assert_eq!(name, "DKT 0004 - Notice of Hearing.pdf");
        // The bytes sit flat under documents/Docket Filings/.
        assert_eq!(
            paths.bytes,
            root.join("documents/Docket Filings/DKT 0004 - Notice of Hearing.pdf")
        );
        assert_eq!(
            std::fs::read(&paths.bytes).expect("bytes on disk"),
            DOCUMENT_BYTES
        );
        // The pointer sits at blobs/<name>.yaml, flat beside every other blob.
        assert_eq!(
            paths.pointer,
            root.join("blobs/DKT 0004 - Notice of Hearing.pdf.yaml")
        );
    }

    #[test]
    fn the_pointer_yaml_matches_the_blob_conventions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ProjectFilingStore::new(dir.path().to_path_buf());
        let FilingOutcome::Filed { name, paths } =
            resolve_filing(&store, &request("4", "Notice of Hearing")).expect("filed")
        else {
            panic!("a fresh name files, it does not dedupe");
        };
        let raw = std::fs::read_to_string(&paths.pointer).expect("pointer on disk");
        // The same parser and contract every blob pointer answers to.
        let pointer = store::document_pointers::DocumentPointer::from_yaml(&raw)
            .expect("the pointer parses and validates");
        assert_eq!(pointer.kind, "pleading", "the Docket Filings default kind");
        assert_eq!(pointer.visibility, "internal");
        assert_eq!(
            pointer.matter_folder(),
            store::document_pointers::MatterFolder::DocketFilings
        );
        // The kind already maps here, so no `folder:` key is recorded —
        // the same shape `project sync` writes for these documents.
        assert!(pointer.folder.is_none());
        assert!(!raw.contains("folder:"));
        let version = &pointer.current_version;
        assert_eq!(version.version, 1);
        assert!(pointer.previous_version.is_none());
        assert_eq!(version.sha256, sha_of(DOCUMENT_BYTES));
        assert_eq!(version.size_bytes, i64::try_from(DOCUMENT_BYTES.len()).expect("small"));
        let parsed_at = chrono::DateTime::parse_from_rfc3339(&version.created_at)
            .expect("an RFC 3339 timestamp");
        assert_eq!(parsed_at.offset().local_minus_utc(), 0, "stamped in UTC");
        let _ = uuid::Uuid::parse_str(version.asset_id.to_string().as_str())
            .expect("a well-formed asset id");
        assert_eq!(name, "DKT 0004 - Notice of Hearing.pdf");
    }

    #[test]
    fn the_project_store_re_files_the_same_document_without_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ProjectFilingStore::new(dir.path().to_path_buf());
        let first = resolve_filing(&store, &request("4", "Notice of Hearing")).expect("filed");
        let FilingOutcome::Filed { paths, .. } = first else {
            panic!("a fresh name files, it does not dedupe");
        };
        let bytes_mtime = std::fs::metadata(&paths.bytes)
            .expect("bytes on disk")
            .modified()
            .expect("a mtime");
        let second =
            resolve_filing(&store, &request("4", "Notice of Hearing")).expect("already filed");
        assert_eq!(
            second,
            FilingOutcome::AlreadyFiled {
                name: "DKT 0004 - Notice of Hearing.pdf".to_owned(),
            }
        );
        let still = std::fs::metadata(&paths.bytes)
            .expect("bytes still on disk")
            .modified()
            .expect("a mtime");
        assert_eq!(bytes_mtime, still, "nothing was rewritten");
    }

    #[test]
    fn the_project_store_escalates_a_real_collision_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = ProjectFilingStore::new(dir.path().to_path_buf());
        resolve_filing(&store, &request("4", "Notice of Hearing")).expect("filed");
        let other =
            FilingRequest::new("4", false, "Amended Notice", "pdf", OTHER_BYTES.to_vec())
                .expect("the document has bytes");
        let second = resolve_filing(&store, &other).expect("filed beside the first");
        assert_eq!(
            second,
            FilingOutcome::Filed {
                name: "DKT 0004-att - Amended Notice.pdf".to_owned(),
                paths: FiledPaths {
                    bytes: dir
                        .path()
                        .join("documents/Docket Filings/DKT 0004-att - Amended Notice.pdf"),
                    pointer: dir
                        .path()
                        .join("blobs/DKT 0004-att - Amended Notice.pdf.yaml"),
                },
            }
        );
    }

    // ---- Intake integration ---------------------------------------------

    #[tokio::test]
    async fn the_dkt_filing_intake_files_through_the_intake_seam() {
        let store = MemoryStore::default();
        let intake = DktFilingIntake::new(&store);
        let receipt = intake
            .enqueue(&scraped("4", "Notice of Hearing"))
            .await
            .expect("filed");
        assert!(!receipt.deduped);
        assert_eq!(
            store
                .files
                .lock()
                .expect("uncontended")
                .get("DKT 0004 - Notice of Hearing.pdf")
                .expect("the bytes were filed"),
            &DOCUMENT_BYTES.to_vec()
        );
        assert!(store
            .pointers
            .lock()
            .expect("uncontended")
            .contains_key("DKT 0004 - Notice of Hearing.pdf"));
    }

    #[tokio::test]
    async fn the_dkt_filing_intake_reports_a_refile_as_deduped() {
        let store = MemoryStore::default();
        let intake = DktFilingIntake::new(&store);
        intake
            .enqueue(&scraped("4", "Notice of Hearing"))
            .await
            .expect("filed");
        let receipt = intake
            .enqueue(&scraped("4", "Notice of Hearing"))
            .await
            .expect("already filed");
        assert!(receipt.deduped, "the same document re-enqueues as deduped");
    }

    #[tokio::test]
    async fn the_dkt_filing_intake_rejects_an_unnameable_document() {
        let store = MemoryStore::default();
        let intake = DktFilingIntake::new(&store);
        let outcome = intake.enqueue(&scraped("not-an-entry", "Notice")).await;
        let error = outcome.expect_err("the entry number cannot be normalized");
        match error {
            IntakeError::Rejected { detail } => {
                assert!(
                    detail.contains("entry_number"),
                    "the error names the naming failure: {detail}"
                );
            }
            IntakeError::Transient { .. } => {
                panic!("a naming failure is terminal, not transient")
            }
        }
        assert!(
            store.files.lock().expect("uncontended").is_empty(),
            "a rejected document files nothing"
        );
    }

    #[tokio::test]
    async fn a_storage_failure_surfaces_as_transient_so_the_run_retries() {
        struct FailingStore;
        impl FilingStore for FailingStore {
            fn sha256_of(&self, _name: &str) -> Result<Option<String>, StorageError> {
                Err(StorageError::Io {
                    path: PathBuf::from("/nowhere"),
                    source: std::io::Error::other("disk offline"),
                })
            }

            fn file(
                &self,
                _name: &str,
                _request: &FilingRequest,
            ) -> Result<FiledPaths, StorageError> {
                panic!("never reached: the hash probe fails first");
            }
        }
        let store = FailingStore;
        let intake = DktFilingIntake::new(&store);
        let outcome = intake.enqueue(&scraped("4", "Notice of Hearing")).await;
        assert!(
            matches!(outcome, Err(IntakeError::Transient { .. })),
            "a storage failure is retryable, not terminal"
        );
    }
}

