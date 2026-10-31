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

