use std::{fmt, path::Path};

use sha2::{Digest, Sha256};

/// Category of an expense supported by a receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpenseCategory {
    CourtFilingFee,
    EFilingServiceFee,
    DepositionAndCourtReporter,
    Transcript,
    CertifiedMailAndPostage,
    ProcessService,
    Other,
}

/// The amount, payee, category, and source the receipt supports.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptMetadata {
    pub amount_cents: i64,
    pub currency: String,
    pub payee: String,
    pub category: ExpenseCategory,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub journal_entry_id: Option<uuid::Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transaction_id: Option<String>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum ReceiptMetadataError {
    #[error("receipt metadata requires kind receipt")]
    Kind,
    #[error(
        "receipt metadata must contain a positive amount and a three-letter uppercase currency"
    )]
    Amount,
    #[error("receipt metadata requires a nonempty payee")]
    Payee,
    #[error("receipt metadata requires exactly one journal entry ID or transaction ID")]
    Reference,
    #[error("receipt metadata has an invalid shape")]
    Shape,
}

impl ReceiptMetadata {
    /// # Errors
    /// Returns the first invalid receipt field or kind.
    pub fn validate(&self, kind: &str) -> Result<(), ReceiptMetadataError> {
        if kind != "receipt" {
            return Err(ReceiptMetadataError::Kind);
        }
        if self.amount_cents <= 0
            || self.currency.len() != 3
            || !self.currency.bytes().all(|byte| byte.is_ascii_uppercase())
        {
            return Err(ReceiptMetadataError::Amount);
        }
        if self.payee.trim().is_empty()
            || self.payee.len() > 256
            || self.payee.chars().any(char::is_control)
        {
            return Err(ReceiptMetadataError::Payee);
        }
        match (self.journal_entry_id, self.transaction_id.as_deref()) {
            (Some(id), None) if !id.is_nil() => Ok(()),
            (None, Some(id))
                if !id.trim().is_empty()
                    && id.len() <= 256
                    && !id.chars().any(char::is_control) =>
            {
                Ok(())
            }
            _ => Err(ReceiptMetadataError::Reference),
        }
    }

    /// Read and validate the receipt block from an Asset's metadata.
    ///
    /// # Errors
    /// The block has an invalid shape, kind, or field.
    pub fn from_asset_metadata(
        kind: &str,
        metadata: Option<&serde_json::Value>,
    ) -> Result<Option<Self>, ReceiptMetadataError> {
        let Some(value) = metadata.and_then(|metadata| metadata.get("receipt")) else {
            return Ok(None);
        };
        let receipt: Self =
            serde_json::from_value(value.clone()).map_err(|_| ReceiptMetadataError::Shape)?;
        receipt.validate(kind)?;
        Ok(Some(receipt))
    }
}

/// Maximum size of one document accepted by the document upload lane.
///
/// Twenty-five MiB matches the size mail systems accept for real instruments
/// while keeping the buffered REST upload bounded until the lane can stream.
/// Keep this limit here so every document-ingest caller shares one contract.
pub const MAX_DOCUMENT_UPLOAD_BYTES: usize = 25 * 1024 * 1024;

/// Maximum size of one transcript document, the kind that carries court
/// hearing audio (LAW-172).
///
/// A two-hour hearing recording docketed as a PDF wrapper runs about 60 MB, and
/// the bytes of a one-time court free look cannot be re-encoded or re-fetched.
/// One hundred MiB leaves room for longer hearings while keeping the buffered
/// base64 request bounded (about 134 MB). Larger media uploads through the
/// resumable streaming lane ([`MAX_STREAMING_UPLOAD_BYTES`]), not this door.
pub const MAX_MEDIA_DOCUMENT_UPLOAD_BYTES: usize = 100 * 1024 * 1024;

/// The document-size limit that applies to a document `kind`.
#[must_use]
pub fn max_document_upload_bytes(kind: &str) -> usize {
    if kind == "transcript" {
        MAX_MEDIA_DOCUMENT_UPLOAD_BYTES
    } else {
        MAX_DOCUMENT_UPLOAD_BYTES
    }
}

/// The largest document the streaming lane accepts.
///
/// One GiB covers court hearing audio (the LAW-172 report is ~60 MB) and
/// video exhibits while bounding the lane's one whole-object materialization:
/// `StorageService::put` writes an object from one buffer, so composing the
/// staged parts at completion holds the full file once. Above this the
/// upload is refused with [`streaming_upload_size_message`] rather than
/// half-bounded.
pub const MAX_STREAMING_UPLOAD_BYTES: usize = 1024 * 1024 * 1024;

/// Explain a refusal past the streaming lane's own cap — the one size the
/// lane itself cannot take.
#[must_use]
pub fn streaming_upload_size_message(actual: usize) -> String {
    format!(
        "Document upload exceeds the streaming lane's maximum of \
         {MAX_STREAMING_UPLOAD_BYTES} bytes; received {actual} bytes. No Navigator \
         upload path accepts more; the file must be split."
    )
}

/// Explain a document-size rejection for `kind`: the limit that applies, the
/// actual size, and where an oversized file should go instead.
#[must_use]
pub fn document_upload_size_message_for_kind(kind: &str, actual: usize) -> String {
    let max = max_document_upload_bytes(kind);
    let streaming = human_bytes(MAX_STREAMING_UPLOAD_BYTES);
    format!(
        "Document upload exceeds the maximum of {max} bytes; received {actual} bytes. Files over \
         this limit, up to {streaming}, upload through the resumable streaming lane, which \
         `hudson site document upload` and `navigator project sync` route automatically."
    )
}

/// A byte count in whole binary units (`500 MiB`, `1 GiB`), the way the
/// limits table and the refusal copy state every limit.
#[must_use]
pub fn human_bytes(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    const GIB: usize = 1024 * MIB;
    if bytes >= GIB && bytes.is_multiple_of(GIB) {
        format!("{} GiB", bytes / GIB)
    } else if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// The Pointer path must retain the uploaded filename's extension.
/// A slug without one cannot be represented as `blobs/<slug>.yaml` while
/// also satisfying the Project gate's pointer rule.
#[derive(Debug, PartialEq, Eq)]
pub enum DocumentSlugError {
    MissingOrMismatchedExtension { filename: String, slug: String },
}

impl fmt::Display for DocumentSlugError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingOrMismatchedExtension { filename, slug } => {
                if let Some(suggested_slug) = suggested_slug(filename, slug) {
                    write!(
                        formatter,
                        "document slug `{slug}` must carry the source extension from `{filename}`; use `{suggested_slug}` so it can be represented as a Pointer"
                    )
                } else {
                    write!(
                        formatter,
                        "document slug `{slug}` must carry the source extension from `{filename}` so it can be represented as a Pointer"
                    )
                }
            }
        }
    }
}

impl std::error::Error for DocumentSlugError {}

/// Reject a document identity that cannot be represented by a repository
/// pointer. Upload callers share this check so a pointer is never issued that
/// the local gate would necessarily reject.
pub fn validate_document_slug(filename: &str, slug: &str) -> Result<(), DocumentSlugError> {
    let filename_extension = Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty());
    let slug_extension = Path::new(slug)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty());

    if filename_extension.is_some_and(|expected| {
        slug_extension.is_some_and(|actual| expected.eq_ignore_ascii_case(actual))
    }) {
        Ok(())
    } else {
        Err(DocumentSlugError::MissingOrMismatchedExtension {
            filename: filename.to_string(),
            slug: slug.to_string(),
        })
    }
}

/// How an executed document was signed: in wet ink on paper, or through
/// `DocuSign`. See the glossary's Signing Method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SigningMethod {
    WetInk,
    Docusign,
}

impl SigningMethod {
    /// Every method, in the order help and errors name them.
    pub const ALL: [Self; 2] = [Self::WetInk, Self::Docusign];

    /// The stored and committed spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WetInk => "wet_ink",
            Self::Docusign => "docusign",
        }
    }

    /// Parse the stored and committed spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str() == value)
    }
}

/// The signing record on an executed document: its [`SigningMethod`] and the
/// calendar date it was signed (`YYYY-MM-DD`). Stored on the `asset` row and
/// carried on its committed pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signing {
    pub method: SigningMethod,
    pub signed_on: chrono::NaiveDate,
}

/// How a notarial acknowledgment was taken: before a notary in person, or
/// by remote online notarization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotarizationMethod {
    InPerson,
    RemoteOnline,
}

impl NotarizationMethod {
    /// Every method, in the order help and errors name them.
    pub const ALL: [Self; 2] = [Self::InPerson, Self::RemoteOnline];

    /// The stored and committed spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InPerson => "in_person",
            Self::RemoteOnline => "remote_online",
        }
    }

    /// Parse the stored and committed spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str() == value)
    }
}

/// One notarial acknowledgment on an executed document. A packet may hold
/// several, so a document carries a list. It records where and how the
/// acknowledgment was taken and when the notary's commission expires, and
/// deliberately never the notary's name or commission number.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Notarization {
    pub method: NotarizationMethod,
    pub acknowledged_on: chrono::NaiveDate,
    /// The two-letter state code where the notary is commissioned, such as `NV`.
    pub state: String,
    pub county: String,
    pub commission_expires: chrono::NaiveDate,
}

/// Why a [`Notarization`] cannot be recorded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotarizationError {
    #[error("notarization state must be a two-letter code such as `NV`, not `{0}`")]
    State(String),
    #[error("notarization county must not be blank")]
    County,
    #[error("the notary's commission must not expire before the acknowledgment date")]
    CommissionExpired,
}

impl Notarization {
    /// Enforce what the field types cannot: a two-letter uppercase state
    /// code, a non-blank county, and a commission still in force on the
    /// acknowledgment date.
    ///
    /// # Errors
    /// The first [`NotarizationError`].
    pub fn validate(&self) -> Result<(), NotarizationError> {
        if self.state.len() != 2 || !self.state.bytes().all(|byte| byte.is_ascii_uppercase()) {
            return Err(NotarizationError::State(self.state.clone()));
        }
        if self.county.trim().is_empty() {
            return Err(NotarizationError::County);
        }
        if self.commission_expires < self.acknowledged_on {
            return Err(NotarizationError::CommissionExpired);
        }
        Ok(())
    }
}

/// How confidently an exhibit is tied to the claim it bears on, in the
/// evidence index's closed vocabulary (LAW-185). `exact` is a link a lawyer
/// verified; `inferred` is a link filed by routing with a human expected to
/// confirm it; `unknown` is everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceConfidence {
    Exact,
    Inferred,
    Unknown,
}

impl EvidenceConfidence {
    /// Every confidence, in the order help and errors name them.
    pub const ALL: [Self; 3] = [Self::Exact, Self::Inferred, Self::Unknown];

    /// The stored and committed spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }

    /// Parse the stored and committed spelling.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|confidence| confidence.as_str() == value)
    }
}

/// The exhibit metadata the matter page's evidence index reads (LAW-185):
/// the date the exhibit itself bears, where it came from, how confidently it
/// is tied to what it supports, and the claim references it bears on. Stored
/// on the `asset` row and carried on its committed pointer. Every field is
/// optional — an exhibit with none recorded still indexes, showing its gaps.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    /// The date the exhibit bears, when it has one — distinct from
    /// `published_at` (a court's file stamp) and `received_at` (when the
    /// bytes arrived).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dated_at: Option<chrono::NaiveDate>,
    /// Where the exhibit came from, as the lawyer knows it — an email, a
    /// message thread, a download. Never a `source` channel: the asset's own
    /// `source` column already records the upload lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// How confidently the exhibit is tied to what it supports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<EvidenceConfidence>,
    /// The claim references the exhibit bears on, such as `¶ 12` or
    /// `Count II — Conversion`. An exhibit bearing on nothing reads back as
    /// an empty list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supports: Vec<String>,
}

/// Why an [`Evidence`] block cannot be recorded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EvidenceError {
    #[error("evidence source must not be blank")]
    Source,
    #[error("evidence claim reference {0} must not be blank")]
    Claim(usize),
}

impl Evidence {
    /// Enforce what the field types cannot: a non-blank source, and
    /// non-blank claim references.
    ///
    /// # Errors
    /// The first [`EvidenceError`].
    pub fn validate(&self) -> Result<(), EvidenceError> {
        if self
            .source
            .as_deref()
            .is_some_and(|source| source.trim().is_empty())
        {
            return Err(EvidenceError::Source);
        }
        for (index, claim) in self.supports.iter().enumerate() {
            if claim.trim().is_empty() {
                return Err(EvidenceError::Claim(index));
            }
        }
        Ok(())
    }
}

/// The content hash every asset and document is stored and deduped under.
///
/// Public so a caller that needs to ask "does this project already hold
/// these bytes?" hashes them exactly the way ingest does — a second
/// implementation would drift and silently stop matching.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

fn suggested_slug(filename: &str, slug: &str) -> Option<String> {
    let extension = Path::new(filename)
        .extension()
        .and_then(|extension| extension.to_str())
        .filter(|extension| !extension.is_empty())?;
    let stem = slug
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .filter(|stem| !stem.is_empty() && !stem.ends_with('/') && !stem.ends_with('\\'))
        .unwrap_or(slug);
    Some(format!("{stem}.{extension}"))
}
