//! File one individually approved Inbox Message Asset as a Project document.

use std::sync::Arc;

use cloud::attachment_scanner::{
    AttachmentScanner, ScanError, ScanVerdict, DEFAULT_MAX_STREAM_BYTES,
};
use cloud::StorageService;
use serde::{Deserialize, Serialize};
use store::application_store::ApplicationStore;
use store::documents::{self, DocumentIdentity, IngestArgs, MailProvenance};
use store::inbox::InboxError;
use store::inbox_triage::{approved_attachment, AttachmentFilingApproval, AttachmentFilingAttempt};
use uuid::Uuid;

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum AttachmentFilingError {
    #[error("the attachment approval is no longer current")]
    ApprovalExpired,
    #[error("the attachment exceeds the scanner size limit")]
    Oversized,
    #[error("the approved attachment does not match the archived bytes")]
    InvalidSource,
    #[error("the attachment failed the malware scan")]
    Malware,
    #[error("the approved attachment could not be filed")]
    Unavailable,
}

fn map_scope(error: &InboxError) -> AttachmentFilingError {
    match error {
        InboxError::NotFound | InboxError::Denied => AttachmentFilingError::ApprovalExpired,
        _ => AttachmentFilingError::Unavailable,
    }
}

/// Scan and file the exact approved asset. Stable mail provenance elects one
/// document across concurrent and repeated executions.
///
/// # Errors
///
/// Refuses stale approvals, mismatched source bytes, oversized data, scanning
/// errors, malware and store failures.
pub async fn file_approved_attachment(
    db: &ApplicationStore,
    storage: &Arc<dyn StorageService>,
    scanner: &dyn AttachmentScanner,
    approval: &AttachmentFilingApproval,
) -> Result<Uuid, AttachmentFilingError> {
    let approved = approved_attachment(db, approval)
        .await
        .map_err(|error| map_scope(&error))?;
    let asset = store::assets::find_by_id(db, approved.source.asset_id)
        .await
        .map_err(|_| AttachmentFilingError::Unavailable)?
        .filter(|asset| {
            asset
                .project_id
                .is_none_or(|project| project == approval.filing.project_id)
                && asset.sha256_hex == approved.source.sha256
        })
        .ok_or(AttachmentFilingError::InvalidSource)?;
    let asset_size =
        usize::try_from(asset.byte_size).map_err(|_| AttachmentFilingError::InvalidSource)?;
    let stored_size = storage
        .head(&asset.storage_key)
        .await
        .map_err(|_| AttachmentFilingError::Unavailable)?
        .ok_or(AttachmentFilingError::Unavailable)?
        .size_bytes;
    let stored_size = usize::try_from(stored_size).map_err(|_| AttachmentFilingError::Oversized)?;
    if asset_size > DEFAULT_MAX_STREAM_BYTES || stored_size > DEFAULT_MAX_STREAM_BYTES {
        return Err(AttachmentFilingError::Oversized);
    }
    if stored_size != asset_size {
        return Err(AttachmentFilingError::InvalidSource);
    }

    let object = storage
        .get(&asset.storage_key)
        .await
        .map_err(|_| AttachmentFilingError::Unavailable)?;
    if object.bytes.len() != asset_size || documents::sha256_hex(&object.bytes) != asset.sha256_hex
    {
        return Err(AttachmentFilingError::InvalidSource);
    }
    match scanner.scan(&object.bytes).await {
        Ok(ScanVerdict::Clean) => {}
        Ok(ScanVerdict::Found { .. }) => return Err(AttachmentFilingError::Malware),
        Err(ScanError::SizeLimit { .. }) => return Err(AttachmentFilingError::Oversized),
        Err(_) => return Err(AttachmentFilingError::Unavailable),
    }

    // Scanning can take tens of seconds. Renew the access and revision check
    // immediately before filing the reviewed bytes.
    approved_attachment(db, approval)
        .await
        .map_err(|error| map_scope(&error))?;
    let message_id = approval.filing.message_id.to_string();
    let index = usize::try_from(approved.source.position)
        .map_err(|_| AttachmentFilingError::InvalidSource)?;
    let received = chrono::DateTime::parse_from_rfc3339(&approved.occurred_at)
        .map_err(|_| AttachmentFilingError::InvalidSource)?;
    let filename = store::mail_attachments::filename_with_extension(&approved.source.filename);
    let slug = store::mail_attachments::slug(&message_id, index, &filename, received.date_naive());
    let filed = documents::ingest_bytes_with_mail_provenance_once(
        db,
        storage,
        &IngestArgs {
            project_id: approval.filing.project_id,
            source: documents::source::EMAIL,
            filename: &filename,
            kind: &approval.kind,
            content_type: &asset.content_type,
            description: None,
            visibility: documents::visibility::INTERNAL,
            secondary_storage_key: None,
        },
        &DocumentIdentity {
            slug: Some(&slug),
            ..Default::default()
        },
        &MailProvenance {
            message_id: &message_id,
            sender: &approved.sender,
            received_at: &approved.occurred_at,
            subject: approved.subject.as_deref().unwrap_or_default(),
        },
        &object.bytes,
    )
    .await
    .map_err(|_| AttachmentFilingError::Unavailable)?;
    Ok(filed.asset_id)
}

pub type AttachmentFilingWorkflow =
    dbos::WorkflowRef<AttachmentFilingAttempt, (), AttachmentFilingError>;

#[derive(Serialize, Deserialize)]
enum Outcome {
    AlreadyFinished,
    Filed(Uuid),
    Failed(AttachmentFilingError),
}

impl AttachmentFilingError {
    fn outcome_code(&self) -> &'static str {
        match self {
            Self::ApprovalExpired => "approval_expired",
            Self::Oversized => "oversized",
            Self::InvalidSource => "invalid_source",
            Self::Malware => "malware",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Bind the worker to an opaque job ID. The application database keeps
/// the approved target, and the workflow checkpoints only IDs and outcomes.
///
/// # Errors
///
/// Propagates the DBOS workflow registration error.
pub fn register_dbos(
    dbos: &dbos::DBOS,
    db: ApplicationStore,
    storage: Arc<dyn StorageService>,
    scanner: Arc<dyn AttachmentScanner>,
) -> dbos::Result<AttachmentFilingWorkflow> {
    dbos.register_workflow("FileInboxAttachment", move |job| {
        let db = db.clone();
        let storage = storage.clone();
        let scanner = scanner.clone();
        async move {
            let result = dbos::step_with(
                "file-approved-inbox-attachment",
                dbos::StepOptions {
                    max_attempts: 3,
                    ..Default::default()
                },
                || async {
                    let approval = store::inbox_triage::pending_attachment_filing(job)
                        .await
                        .map_err(|_| {
                            dbos::Error::Application(AttachmentFilingError::Unavailable)
                        })?;
                    let Some(approval) = approval else {
                        return Ok(Outcome::AlreadyFinished);
                    };
                    match file_approved_attachment(&db, &storage, scanner.as_ref(), &approval).await
                    {
                        Ok(asset) => Ok(Outcome::Filed(asset)),
                        Err(AttachmentFilingError::Unavailable) => {
                            Err(dbos::Error::Application(AttachmentFilingError::Unavailable))
                        }
                        Err(error) => Ok(Outcome::Failed(error)),
                    }
                },
            )
            .await;
            let result = match result {
                Ok(Outcome::AlreadyFinished) => return Ok(()),
                Ok(Outcome::Filed(asset)) => Ok(asset),
                Ok(Outcome::Failed(error)) | Err(dbos::Error::Application(error)) => {
                    Err(error.outcome_code())
                }
                Err(error) => return Err(error),
            };
            dbos::step_with(
                "record-inbox-attachment-filing",
                dbos::StepOptions {
                    max_attempts: 3,
                    ..Default::default()
                },
                || async {
                    store::inbox_triage::finish_attachment_filing(job, result)
                        .await
                        .map_err(|_| dbos::Error::Application(AttachmentFilingError::Unavailable))
                },
            )
            .await
        }
    })
}

/// Recover persisted submissions after a worker outage or restart.
pub async fn schedule(workflow: AttachmentFilingWorkflow, stop: dbos::CancellationToken) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = stop.cancelled() => break,
            _ = tick.tick() => reconcile(&workflow).await,
        }
    }
}

async fn reconcile(workflow: &AttachmentFilingWorkflow) {
    let Ok(ids) = store::inbox_triage::pending_attachment_filings(50).await else {
        tracing::warn!("could not read pending Inbox attachment filings");
        return;
    };
    for job in ids {
        let workflow_id = format!("FileInboxAttachment:{}:{}", job.id, job.attempt);
        let started = workflow
            .start_with(
                job,
                dbos::StartOptions {
                    workflow_id: Some(&workflow_id),
                    ..Default::default()
                },
            )
            .await;
        let Ok(handle) = started else {
            tracing::warn!(job_id = %job.id, "could not submit the Inbox attachment filing");
            continue;
        };
        if handle
            .status()
            .await
            .is_ok_and(|status| !status.is_active())
            && store::inbox_triage::finish_attachment_filing(job, Err("unavailable"))
                .await
                .is_err()
        {
            tracing::warn!(job_id = %job.id, "could not reconcile a stopped attachment filing");
        }
    }
}
