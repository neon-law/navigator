//! The outbound document hook: a fetched, hashed document handed into
//! Navigator's omnichannel inbox — the same entry point support-app intake
//! uses, and the only path toward a matter folder. This PR defines the
//! interface and ships the test recording double; wiring the real inbox
//! source is its own task.
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
    #[cfg_attr(not(test), allow(dead_code))] // raised by the real inbox integration later
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

/// The production placeholder behind the intake seam: the omnichannel
/// inbox source for scraped documents is its own integration task, and no
/// site adapter is registered yet either. Until both land, this is what a
/// run hands documents to, and it says so honestly instead of dropping
/// bytes on the floor.
#[derive(Debug, Default)]
pub(crate) struct UnwiredIntake;

#[async_trait::async_trait]
impl InboxIntake for UnwiredIntake {
    async fn enqueue(&self, document: &ScrapedDocument) -> Result<IntakeReceipt, IntakeError> {
        Err(IntakeError::Rejected {
            detail: format!(
                "the omnichannel intake for scraped documents is not wired yet; `{}` is staged but was not delivered",
                document.filename
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{InboxIntake, IntakeError, RecordingIntake, ScrapedDocument, UnwiredIntake};
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
    async fn the_unwired_intake_rejects_and_names_the_staged_document() {
        // The production placeholder must say honestly that bytes were
        // staged but not delivered — never drop them silently.
        let outcome = UnwiredIntake
            .enqueue(&document("4", "Notice of Hearing"))
            .await;
        let error = outcome.expect_err("the placeholder is not wired");
        match error {
            IntakeError::Rejected { detail } => {
                assert!(
                    detail.contains("DKT 0004 - Notice of Hearing.pdf"),
                    "the error names the staged document: {detail}"
                );
                assert!(
                    detail.contains("not wired"),
                    "the error says the intake is not wired: {detail}"
                );
            }
            IntakeError::Transient { .. } => {
                panic!("expected a Rejected intake error, got a Transient one")
            }
        }
    }
}
