//! The outbound document hook: a fetched, hashed document handed into
//! Navigator's omnichannel inbox — the same entry point support-app intake
//! uses, and the only path toward a matter folder. Production hands
//! documents to [`StoreInboxIntake`] (an inbox message with the bytes
//! stored as a content-addressed Asset); tests record with
//! [`RecordingIntake`].
//!
//! Naming is Navigator's own, not the scraper's: every document carries the
//! `DKT NNNN[-att] - <title>.<ext>` name `store::cases::filing_name`
//! prescribes, so a notice from any site looks exactly like a docket
//! filing once triage files it.

/// Why a document could not be named for filing.
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub(crate) struct NamingError(#[from] store::cases::FilingNameError);

/// Why the intake could not accept a document.
#[derive(Debug, thiserror::Error)]
pub(crate) enum IntakeError {
    #[error("the omnichannel inbox rejected the document: {detail}")]
    Rejected { detail: String },
    #[error("the omnichannel inbox is temporarily unavailable: {detail}")]
    Transient { detail: String },
}

/// A document the framework scraped, staged and hashed, ready to enqueue.
///
/// `Debug` shows the filename and sizes, never the document bytes —
/// transcripts are shared; document contents are client material.
#[derive(Clone)]
pub(crate) struct ScrapedDocument {
    pub(crate) site: crate::scraper::adapter::SiteKey,
    pub(crate) docket: String,
    /// The entry number as the site printed it, for the filing path that
    /// derives the DKT name itself.
    pub(crate) entry_number: String,
    /// The entry's title, verbatim as the docket prints it.
    pub(crate) title: String,
    pub(crate) extension: String,
    /// The `DKT NNNN[-att] - <title>.<ext>` name Navigator prescribes.
    pub(crate) filename: String,
    pub(crate) sha256: String,
    pub(crate) bytes: Vec<u8>,
}

impl ScrapedDocument {
    /// Name a staged download per the DKT convention and wrap it.
    ///
    /// # Errors
    ///
    /// When the court's entry number cannot be normalized to the
    /// prescribed `DKT NNNN[-att]` form.
    pub(crate) fn from_download(
        site: crate::scraper::adapter::SiteKey,
        docket: &str,
        notice: &crate::scraper::adapter::NoticeRef,
        sha256: String,
        bytes: Vec<u8>,
    ) -> Result<Self, NamingError> {
        let filename =
            store::cases::filing_name(&notice.entry_number, &notice.title, &notice.extension)?;
        Ok(Self {
            site,
            docket: docket.to_owned(),
            entry_number: notice.entry_number.clone(),
            title: notice.title.clone(),
            extension: notice.extension.clone(),
            filename,
            sha256,
            bytes,
        })
    }
}

impl std::fmt::Debug for ScrapedDocument {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScrapedDocument")
            .field("site", &self.site)
            .field("docket", &self.docket)
            .field("entry_number", &self.entry_number)
            .field("title", &self.title)
            .field("extension", &self.extension)
            .field("filename", &self.filename)
            .field("sha256", &self.sha256)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// What the intake returned for one enqueued document: the inbox message it
/// became, and whether the inbox recognized the document as already
/// delivered (a retry after a lost ack).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IntakeReceipt {
    pub message_id: uuid::Uuid,
    pub deduped: bool,
}

/// The outbound seam between the scraper and Navigator's omnichannel
/// inbox. Production implements this over the inbox source the support app
/// already feeds; tests record with [`RecordingIntake`].
#[async_trait::async_trait]
pub(crate) trait InboxIntake: Send + Sync {
    /// Hand one scraped document to the omnichannel inbox.
    ///
    /// # Errors
    ///
    /// `Transient` failures may be retried by the orchestration; anything
    /// else is terminal for the run.
    async fn enqueue(&self, document: &ScrapedDocument) -> Result<IntakeReceipt, IntakeError>;
}

/// The test double: records every document handed across the seam and can
/// be scripted to reject transiently, so orchestration tests exercise the
/// retry path without an inbox.
#[derive(Debug, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct RecordingIntake {
    received: std::sync::Mutex<Vec<ScrapedDocument>>,
    /// The first N `enqueue` calls fail with [`IntakeError::Transient`].
    pub(crate) transient_failures_first: std::sync::atomic::AtomicU32,
    calls: std::sync::atomic::AtomicU32,
}

impl RecordingIntake {
    /// Snapshots, in enqueue order, everything the intake accepted.
    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn received(&self) -> Vec<ScrapedDocument> {
        self.received.lock().expect("uncontended").clone()
    }

    #[must_use]
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn calls(&self) -> u32 {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl InboxIntake for RecordingIntake {
    async fn enqueue(&self, document: &ScrapedDocument) -> Result<IntakeReceipt, IntakeError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let failures = self
            .transient_failures_first
            .load(std::sync::atomic::Ordering::SeqCst);
        if call < failures {
            return Err(IntakeError::Transient {
                detail: "the inbox is not answering (scripted)".to_owned(),
            });
        }
        self.received
            .lock()
            .expect("uncontended")
            .push(document.clone());
        Ok(IntakeReceipt {
            message_id: uuid::Uuid::now_v7(),
            deduped: false,
        })
    }
}

/// An intake double that always fails with a transient error — for tests
/// of the retry budget and of the boundary between staging and enqueue.
#[derive(Debug, Default)]
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct FailingIntake;

#[async_trait::async_trait]
impl InboxIntake for FailingIntake {
    async fn enqueue(&self, document: &ScrapedDocument) -> Result<IntakeReceipt, IntakeError> {
        let _ = document;
        Err(IntakeError::Transient {
            detail: "the inbox service is restarting".to_owned(),
        })
    }
}

/// The production intake: a scraped document becomes an omnichannel inbox
/// message on the `scraper` channel — the same entry point the support app
/// feeds — with the document bytes stored as a content-addressed Asset and
/// attached, so triage sees an ordinary inbox row. The filing path is
/// triage's, never the scraper's.
///
/// Threading: the inbox threads on `provider_thread_ref`, so one
/// conversation collects every notice a site scraped for one docket.
/// Idempotence: the inbox's dedupe key (Firm + channel + account +
/// provider message id) is deterministic per notice, so a rerun of the
/// same docket rediscovers the original delivery (`deduped: true`) rather
/// than duplicating the row.
pub(crate) struct StoreInboxIntake {
    db: std::sync::Arc<store::surreal::SurrealDb>,
    storage: std::sync::Arc<dyn cloud::StorageService>,
    firm_id: uuid::Uuid,
}

impl StoreInboxIntake {
    /// Wire the intake to a Firm's inbox over the given store and asset
    /// storage (GCS in deployment, the filesystem emulator in dev).
    pub(crate) fn new(
        db: std::sync::Arc<store::surreal::SurrealDb>,
        storage: std::sync::Arc<dyn cloud::StorageService>,
        firm_id: uuid::Uuid,
    ) -> Self {
        Self {
            db,
            storage,
            firm_id,
        }
    }
}

/// A storage or database hiccup is transient; a semantic refusal is not.
fn intake_error(error: store::inbox::InboxError) -> IntakeError {
    match error {
        store::inbox::InboxError::Db(detail) => IntakeError::Transient {
            detail: format!("the omnichannel store failed: {detail}"),
        },
        other => IntakeError::Rejected {
            detail: other.to_string(),
        },
    }
}

/// The MIME type an attachment carries, from the document's extension.
fn content_type_for(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[async_trait::async_trait]
impl InboxIntake for StoreInboxIntake {
    async fn enqueue(&self, document: &ScrapedDocument) -> Result<IntakeReceipt, IntakeError> {
        // The bytes become a content-addressed Asset first — the same
        // store and dedupe-by-sha the support app's attachments use — so
        // the attachment link row's key always matches the stored content.
        let asset_id = store::assets::ingest_content(
            self.db.as_ref(),
            &self.storage,
            &document.bytes,
            content_type_for(&document.extension),
        )
        .await
        .map_err(|error| IntakeError::Transient {
            detail: format!("storing the document asset failed: {error}"),
        })?;

        let provider_message_id = format!(
            "{}:{}:{}",
            document.site.as_str(),
            document.docket,
            document.entry_number
        );
        let body = format!(
            "Scraped from {}: docket {}, entry {} — {} ({} bytes).",
            document.site.as_str(),
            document.docket,
            document.entry_number,
            document.title,
            document.bytes.len()
        );
        let occurred_at = chrono::Utc::now().to_rfc3339();
        let ingested = store::inbox::ingest_inbound(
            self.db.as_ref(),
            &store::inbox::IngestInbound {
                // No project attribution at ingest time: like support-app
                // intake, the conversation stays on the Firm until triage
                // assigns it to the docket task's Project.
                project_id: None,
                firm_id: self.firm_id,
                brand_key: None,
                interaction_type: Some("message"),
                provider_post_id: None,
                parent_provider_message_id: None,
                visibility: None,
                channel: store::inbox::channel::SCRAPER,
                provider_account: document.site.as_str(),
                provider_message_id: &provider_message_id,
                provider_thread_ref: Some(&document.docket),
                external_address: document.site.as_str(),
                external_name: Some(document.site.as_str()),
                subject: Some(&document.filename),
                body: &body,
                source_page: None,
                source_channel: Some(store::inbox::channel::SCRAPER),
                matter_consent: false,
                promotional_opt_in: false,
                email_conversation_id: None,
                attachments: vec![store::inbox::IngestAttachment {
                    asset_id,
                    filename: document.filename.clone(),
                    sha256: document.sha256.clone(),
                }],
                occurred_at: &occurred_at,
            },
        )
        .await
        .map_err(intake_error)?;
        Ok(IntakeReceipt {
            message_id: ingested.message_id,
            deduped: ingested.deduped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{InboxIntake, IntakeError, RecordingIntake, ScrapedDocument, StoreInboxIntake};
    use crate::scraper::adapter::{NoticeRef, SiteKey};

    fn site() -> SiteKey {
        SiteKey::new("la-superior").expect("valid key")
    }

    fn notice(entry: &str, title: &str) -> NoticeRef {
        NoticeRef {
            entry_number: entry.to_owned(),
            title: title.to_owned(),
            url: "https://portal.example.com/doc".to_owned(),
            extension: "pdf".to_owned(),
        }
    }

    fn document(entry: &str, title: &str) -> ScrapedDocument {
        ScrapedDocument::from_download(
            site(),
            "0004",
            &notice(entry, title),
            "abc123".to_owned(),
            b"document bytes".to_vec(),
        )
        .expect("the entry number is normalizable")
    }

    #[test]
    fn documents_take_the_dkt_filing_name() {
        let document = document("4", "Notice of Hearing");
        assert_eq!(document.filename, "DKT 0004 - Notice of Hearing.pdf");
    }

    #[test]
    fn attachment_suffixes_survive_naming() {
        let document = document("29-1", "Exhibit A");
        assert_eq!(document.filename, "DKT 0029-1 - Exhibit A.pdf");
    }

    #[test]
    fn debug_shows_sizes_never_bytes() {
        let document = document("4", "Notice of Hearing");
        let rendered = format!("{document:?}");
        assert!(rendered.contains("14"), "the byte length shows");
        assert!(!rendered.contains("document bytes"), "the bytes never show");
    }

    #[tokio::test]
    async fn the_recording_intake_accepts_and_records() {
        let intake = RecordingIntake::default();
        let receipt = intake.enqueue(&document("4", "Notice of Hearing")).await;
        let receipt = receipt.expect("accepted");
        assert!(!receipt.deduped);
        assert_eq!(intake.received().len(), 1);
        assert_eq!(
            intake.received()[0].filename,
            "DKT 0004 - Notice of Hearing.pdf"
        );
        assert_eq!(intake.calls(), 1);
    }

    #[tokio::test]
    async fn a_scripted_transient_failure_then_recovery() {
        let intake = RecordingIntake {
            transient_failures_first: std::sync::atomic::AtomicU32::new(1),
            ..RecordingIntake::default()
        };
        let first = intake.enqueue(&document("4", "Notice of Hearing")).await;
        assert!(matches!(first, Err(IntakeError::Transient { .. })));
        assert!(
            intake.received().is_empty(),
            "a rejected document is not recorded"
        );
        let second = intake.enqueue(&document("4", "Notice of Hearing")).await;
        assert!(second.is_ok());
        assert_eq!(intake.received().len(), 1);
        assert_eq!(intake.calls(), 2);
    }

    #[tokio::test]
    async fn the_store_intake_enqueues_a_document_into_the_inbox() {
        // The production intake against a real embedded store and the
        // filesystem storage: the document becomes an omnichannel message
        // on the scraper channel, and the same notice redelivered — a
        // retried run, a lost ack — is the original row, not a duplicate.
        let db = std::sync::Arc::new(store::surreal::test_support::mem().await);
        let storage_root = tempfile::tempdir().expect("a storage root");
        let storage: std::sync::Arc<dyn cloud::StorageService> = std::sync::Arc::new(
            cloud::FsStorage::new(storage_root.path().to_path_buf())
                .await
                .expect("the filesystem storage opens"),
        );
        let firm_id = store::test_support::seed_firm_with_brand(db.as_ref(), "neon").await;
        let intake = StoreInboxIntake::new(db, storage, firm_id);

        let first = intake
            .enqueue(&document("4", "Notice of Hearing"))
            .await
            .expect("the inbox accepts the document");
        assert!(!first.deduped);

        let second = intake
            .enqueue(&document("4", "Notice of Hearing"))
            .await
            .expect("the redelivery is accepted");
        assert!(second.deduped, "a redelivered notice dedupes");
        assert_eq!(second.message_id, first.message_id);
    }
}
