//! The append-only outbound-email audit trail in PostgreSQL.
//!
//! Each delivery attempt retains its recipient, content, provider outcome and optional
//! Person and Project links. Ordinary reads and writes require validated application
//! history. Explicit captured-history imports use the independent migration adapter.
//! The database refuses changes except the export-gated retention purge.
//!
//! Pagination counts and fetches rows in one read-only repeatable-read transaction.
//! Both queries use the same Project scope and stable newest-first order.

use crate::postgres::PostgresDb;
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

pub mod postgres;

/// One journaled outbound message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SentEmail {
    pub id: Uuid,
    pub recipient: String,
    pub subject: String,
    /// The `From:` actually used, so the trail reflects what the
    /// backend sent rather than what the default was.
    pub sender: String,
    /// The matter this send belongs to, when one is known. Firm-wide messages
    /// and historical rows without matter attribution carry `None`.
    pub project_id: Option<Uuid>,
    /// The Person the message was addressed to, when the caller named one
    /// (`OutboundEmail::person_id`).
    pub person_id: Option<Uuid>,
    /// Slug of the template that rendered the body (`welcome`, …).
    /// `None` for ad-hoc messages.
    pub template_slug: Option<String>,
    pub body: String,
    /// `sent` on success, `failed:<reason>` on failure.
    pub outcome: String,
    /// SendGrid's `X-Message-Id`, captured on a 202. `None` for failed
    /// sends and for the capturing dev backend.
    pub sg_message_id: Option<String>,
    pub sent_at: DateTime<Utc>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Everything [`record`] needs to journal one attempt. A struct rather
/// than eight positional arguments, three of which are `Option<String>`.
#[derive(Debug, Clone)]
pub struct NewSentEmail {
    pub recipient: String,
    pub subject: String,
    pub sender: String,
    pub project_id: Option<Uuid>,
    pub person_id: Option<Uuid>,
    pub template_slug: Option<String>,
    pub body: String,
    pub outcome: String,
    pub sg_message_id: Option<String>,
    pub sent_at: DateTime<Utc>,
}

/// Errors reading or writing the audit trail.
#[derive(Debug, thiserror::Error)]
pub enum SentEmailError {
    /// PostgreSQL configuration, connection or application history is unavailable.
    #[error("PostgreSQL: {0}")]
    Postgres(#[from] crate::postgres::PostgresError),
    /// A PostgreSQL statement failed.
    #[error("PostgreSQL query: {0}")]
    Sql(#[from] sqlx::Error),
}

/// One page of the audit trail, with the totals that describe the same
/// snapshot the rows came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The rows on this page, newest first.
    pub rows: Vec<SentEmail>,
    /// How many pages exist. At least 1, so an empty table still reads
    /// as "Page 1 of 1" rather than "Page 1 of 0".
    pub total_pages: u64,
    /// The page actually returned — the request clamped into range, so
    /// an out-of-range `?page=` yields the final page and says so.
    pub page: u64,
}

/// Journal one outbound attempt after native application admission.
///
/// # Errors
/// PostgreSQL history or insertion is unavailable. The caller records an audit miss
/// separately because delivery has already been attempted.
pub async fn record(new: &NewSentEmail) -> Result<SentEmail, SentEmailError> {
    postgres::record(&PostgresDb::application().await?, new).await
}

/// Read a Project-scoped page and count from the same database snapshot.
/// `None` selects the complete trail; an empty Project set selects no rows.
/// Page numbers and page sizes are clamped to at least one.
///
/// # Errors
/// PostgreSQL application history or the scoped read is unavailable.
pub async fn page(
    requested: u64,
    per_page: u64,
    project_ids: Option<&[Uuid]>,
) -> Result<Page, SentEmailError> {
    postgres::page(
        &PostgresDb::application().await?,
        requested,
        per_page,
        project_ids,
    )
    .await
}

/// Every journaled message, newest first with descending identity as the tie breaker.
///
/// # Errors
/// PostgreSQL application history or the read is unavailable.
pub async fn all() -> Result<Vec<SentEmail>, SentEmailError> {
    postgres::all(&PostgresDb::application().await?).await
}

#[cfg(test)]
#[path = "sent_emails/native_tests.rs"]
mod native_tests;
