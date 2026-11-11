//! `forgejo_push_webhook_journal` — the durable record of verified Forgejo
//! push webhook receipts, and the reads the boot-time replay sweep drains by.
//!
//! # Journal first, consume second
//!
//! The inbound push webhook (`portal::forgejo_webhook`) enters the push
//! pipeline from Forgejo's Git backend. The same discipline the e-signature
//! journal ([`crate::signature_webhook_journal`]) established holds here: a
//! verified, parseable receipt is written **before** any consumption; a
//! consumption failure leaves the row `pending` or `failed` with the failure
//! recorded; the boot-time sweep re-runs re-consumable receipts through the
//! same functions the handler used. A delivery is recovered from the journal,
//! not lost.
//!
//! Only *verified* payloads are journaled. HMAC verification refuses an
//! unverified delivery before any write — this table is the record of what
//! the trusted forge said, not of everything that hit the endpoint.
//!
//! A Forgejo redelivery re-sends the same body under a new delivery id, so
//! idempotency is content-based: the unique event index covers the
//! (repository, ref updates) pair [`push_event_key`] derives, and a
//! redelivery resolves to the existing row instead of double-journaling.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value as Json;
use sqlx::{AssertSqlSafe, Postgres, Row};
use uuid::Uuid;

use super::PostgresDb;

/// The table these rows live in.
const TABLE: &str = "forgejo_push_webhook_journal";

/// The receipt's own unique index: one row per push event. A Forgejo
/// redelivery of the same push resolves here instead of double-journaling,
/// and the replay sweep never sees a duplicate.
const EVENT_INDEX: &str = "forgejo_push_webhook_journal_event";

/// The projection every read shares, so one field list describes the row
/// and a new column cannot reach [`receipt`] from only one query.
const SELECT: &str = "id, delivered_at, repo_full_name, ref_updates, event_key, \
     payload, status, failure_detail, change_id, author, description_summary, \
     stats, conflict_signal, inserted_at, updated_at";

/// A receipt's lifecycle. `pending` is a verified receipt not yet consumed;
/// `analyzed` carries the analysis columns; `delivered` and `skipped` are
/// terminal; `failed` names its last consumption failure and is
/// re-consumable by the replay sweep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Pending,
    Analyzed,
    Delivered,
    Failed,
    Skipped,
}

impl ReceiptStatus {
    /// The stored string — the SQL `CHECK` constraint's closed set.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReceiptStatus::Pending => "pending",
            ReceiptStatus::Analyzed => "analyzed",
            ReceiptStatus::Delivered => "delivered",
            ReceiptStatus::Failed => "failed",
            ReceiptStatus::Skipped => "skipped",
        }
    }

    /// `None` for a string outside the closed set.
    #[must_use]
    pub fn parse(status: &str) -> Option<ReceiptStatus> {
        match status {
            "pending" => Some(ReceiptStatus::Pending),
            "analyzed" => Some(ReceiptStatus::Analyzed),
            "delivered" => Some(ReceiptStatus::Delivered),
            "failed" => Some(ReceiptStatus::Failed),
            "skipped" => Some(ReceiptStatus::Skipped),
            _ => None,
        }
    }

    /// A terminal status takes no further transition — a redelivery of a
    /// `delivered` or `skipped` event re-runs nothing.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, ReceiptStatus::Delivered | ReceiptStatus::Skipped)
    }
}

/// One verified Forgejo push receipt: what the forge said, and where the
/// pipeline has taken it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ForgejoPushReceipt {
    pub id: Uuid,
    /// When Forgejo delivered the push — the webhook's own date, or the
    /// receipt time when the delivery carries none.
    pub delivered_at: DateTime<Utc>,
    /// The pushed repository's full name, `{owner}/{repo}` as Forgejo
    /// addresses it — the `{firm-slug}/{project-code}` namespace.
    pub repo_full_name: String,
    /// The pushed ref updates as their JSON array — for a jj push, the
    /// `push/<change-id>` bookmark's `ref`, `before`, and `after`.
    pub ref_updates: Json,
    /// The content-derived identity [`push_event_key`] computed — the key
    /// a redelivery of the same push deduplicates on.
    pub event_key: String,
    /// The verified payload as its exact JSON text, so replay re-runs the
    /// bytes rather than an interpretation of them.
    pub payload: String,
    pub status: ReceiptStatus,
    /// The last consumption failure, if any — the replay report's first
    /// line. Cleared when a later transition succeeds.
    pub failure_detail: Option<String>,
    /// The analysis columns, `None` until analysis fills them. Persisted
    /// with the receipt so a delivery crash after analysis never re-reads
    /// the Forgejo API.
    pub change_id: Option<String>,
    pub author: Option<String>,
    pub description_summary: Option<String>,
    pub stats: Option<Json>,
    pub conflict_signal: Option<bool>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The per-change analysis the receiver's consumer fills in: change ID
/// (from the `push/*` bookmark name), author, subject, file stats, and the
/// conflict signal.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PushAnalysis {
    pub change_id: String,
    pub author: String,
    pub description_summary: String,
    pub stats: Json,
    pub conflict_signal: bool,
}

/// A verified push delivery, ready to journal. The caller has already
/// verified the HMAC over `payload`'s exact bytes; nothing unverified
/// reaches this table.
#[derive(Debug, Clone, PartialEq)]
pub struct NewPushReceipt {
    pub delivered_at: DateTime<Utc>,
    pub repo_full_name: String,
    pub ref_updates: Json,
    pub payload: String,
}

/// The stable identity of a push event: the repository plus its ref
/// updates. Forgejo's redeliveries re-send the same body under a new
/// delivery id, so the journal deduplicates on content, not on delivery
/// metadata. `serde_json::Value` orders object keys deterministically, so
/// the same body always derives the same key.
#[must_use]
pub fn push_event_key(repo_full_name: &str, ref_updates: &Json) -> String {
    format!("{repo_full_name}\u{1f}{ref_updates}")
}

/// A unique-index violation on the receipt's event index — a redelivery of
/// a push that is already journaled.
fn is_event_conflict(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::constraint)
        == Some(EVENT_INDEX)
}

/// Decode one receipt row. A status outside the closed set — written by
/// something that bypassed this module's guarded transitions — is a decode
/// error naming the status, never a receipt masquerading as one.
fn receipt(row: &sqlx::postgres::PgRow) -> Result<ForgejoPushReceipt, sqlx::Error> {
    let status: String = row.try_get("status")?;
    let status = ReceiptStatus::parse(&status).ok_or_else(|| sqlx::Error::ColumnDecode {
        index: "status".into(),
        source: Box::new(std::io::Error::other(format!(
            "unknown persisted push receipt status: {status}"
        ))),
    })?;
    Ok(ForgejoPushReceipt {
        status,
        id: row.try_get("id")?,
        delivered_at: row.try_get("delivered_at")?,
        repo_full_name: row.try_get("repo_full_name")?,
        ref_updates: row.try_get("ref_updates")?,
        event_key: row.try_get("event_key")?,
        payload: row.try_get("payload")?,
        failure_detail: row.try_get("failure_detail")?,
        change_id: row.try_get("change_id")?,
        author: row.try_get("author")?,
        description_summary: row.try_get("description_summary")?,
        stats: row.try_get("stats")?,
        conflict_signal: row.try_get("conflict_signal")?,
        inserted_at: row.try_get("inserted_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn read_one(
    row: Option<&sqlx::postgres::PgRow>,
) -> Result<Option<ForgejoPushReceipt>, sqlx::Error> {
    row.map(receipt).transpose()
}

/// Journal a verified push receipt **before** it is consumed. Idempotent on
/// the event: a redelivery returns the existing row rather than inserting a
/// duplicate — a concurrent double-delivery loses the event index and
/// re-reads the winner's row.
///
/// # Errors
///
/// [`sqlx::Error`] if the write or the conflict read-back fails.
pub async fn journal_verified_push(
    db: &PostgresDb,
    delivery: &NewPushReceipt,
) -> Result<ForgejoPushReceipt, sqlx::Error> {
    let event_key = push_event_key(&delivery.repo_full_name, &delivery.ref_updates);
    let inserted = sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {TABLE} (id, delivered_at, repo_full_name, ref_updates, event_key, \
             payload, status, inserted_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'pending', now(), now()) RETURNING {SELECT}"
    )))
    .bind(Uuid::now_v7())
    .bind(delivery.delivered_at)
    .bind(&delivery.repo_full_name)
    .bind(&delivery.ref_updates)
    .bind(&event_key)
    .bind(&delivery.payload)
    .fetch_one(db.pool())
    .await;

    match inserted {
        Ok(row) => receipt(&row),
        Err(error) if is_event_conflict(&error) => by_event_key(db, &event_key)
            .await?
            .ok_or(sqlx::Error::RowNotFound),
        Err(error) => Err(error),
    }
}

/// The receipt for one push event, if any — the redelivery lookup. The
/// unique event index enforces at most one row per key, so a caller holding
/// a returned receipt knows a redelivery of the same push resolves to this
/// same row.
///
/// # Errors
///
/// [`sqlx::Error`] if the lookup fails.
pub async fn by_event_key(
    db: &PostgresDb,
    event_key: &str,
) -> Result<Option<ForgejoPushReceipt>, sqlx::Error> {
    read_one(
        sqlx::query(AssertSqlSafe(format!(
            "SELECT {SELECT} FROM {TABLE} WHERE event_key = $1 LIMIT 1"
        )))
        .bind(event_key)
        .fetch_optional(db.pool())
        .await?
        .as_ref(),
    )
}

/// The journal's re-consumable receipts — `pending` or `failed` — oldest
/// first: the replay sweep's work list. `limit` bounds one sweep's work so
/// a restart with a large backlog converges over several boots instead of
/// holding the boot host.
///
/// # Errors
///
/// [`sqlx::Error`] if the read fails.
pub async fn reconsumable_receipts(
    db: &PostgresDb,
    limit: i64,
) -> Result<Vec<ForgejoPushReceipt>, sqlx::Error> {
    let rows = sqlx::query(AssertSqlSafe(format!(
        "SELECT {SELECT} FROM {TABLE} WHERE status IN ('pending', 'failed') \
         ORDER BY inserted_at ASC LIMIT $1"
    )))
    .bind(limit)
    .fetch_all(db.pool())
    .await?;
    rows.iter().map(receipt).collect::<Result<Vec<_>, _>>()
}

/// The state machine's one guarded update: a transition only from the
/// statuses it may start from, and `None` — answered from the row, never
/// re-run — when the row is terminal or absent.
async fn transition(
    db: &PostgresDb,
    statement: sqlx::query::Query<'_, Postgres, sqlx::postgres::PgArguments>,
) -> Result<Option<ForgejoPushReceipt>, sqlx::Error> {
    read_one(statement.fetch_optional(db.pool()).await?.as_ref())
}

/// Record the analysis on a receipt: the per-change facts plus the
/// `analyzed` status. Persisted before delivery so a crash after analysis
/// never re-reads the Forgejo API.
///
/// # Errors
///
/// [`sqlx::Error`] if the write fails.
pub async fn record_analysis(
    db: &PostgresDb,
    receipt_id: Uuid,
    analysis: &PushAnalysis,
) -> Result<Option<ForgejoPushReceipt>, sqlx::Error> {
    transition(
        db,
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE {TABLE} SET status = 'analyzed', updated_at = now(), \
             change_id = $2, author = $3, description_summary = $4, stats = $5, \
             conflict_signal = $6, failure_detail = NULL \
             WHERE id = $1 AND status IN ('pending', 'failed') RETURNING {SELECT}"
        )))
        .bind(receipt_id)
        .bind(&analysis.change_id)
        .bind(&analysis.author)
        .bind(&analysis.description_summary)
        .bind(&analysis.stats)
        .bind(analysis.conflict_signal),
    )
    .await
}

/// Mark a receipt delivered: Slack answered. A redelivery of the same event
/// after this point is answered from the journal, not re-consumed.
///
/// # Errors
///
/// [`sqlx::Error`] if the write fails.
pub async fn mark_delivered(
    db: &PostgresDb,
    receipt_id: Uuid,
) -> Result<Option<ForgejoPushReceipt>, sqlx::Error> {
    transition(
        db,
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE {TABLE} SET status = 'delivered', updated_at = now(), \
             failure_detail = NULL \
             WHERE id = $1 AND status IN ('pending', 'analyzed', 'failed') \
             RETURNING {SELECT}"
        )))
        .bind(receipt_id),
    )
    .await
}

/// Mark a verified receipt skipped: its payload named no `push/*` ref
/// update, so it is observable and never analyzed. Terminal — a redelivery
/// re-runs nothing.
///
/// # Errors
///
/// [`sqlx::Error`] if the write fails.
pub async fn mark_skipped(
    db: &PostgresDb,
    receipt_id: Uuid,
) -> Result<Option<ForgejoPushReceipt>, sqlx::Error> {
    transition(
        db,
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE {TABLE} SET status = 'skipped', updated_at = now(), \
             failure_detail = NULL \
             WHERE id = $1 AND status IN ('pending', 'failed') RETURNING {SELECT}"
        )))
        .bind(receipt_id),
    )
    .await
}

/// Record a failed consumption attempt: the failure is named, and the row
/// becomes `failed` — re-consumable by the next sweep, not lost. A recovery
/// that silently dropped the error would be a journal that lies.
///
/// # Errors
///
/// [`sqlx::Error`] if the write fails.
pub async fn record_failure(
    db: &PostgresDb,
    receipt_id: Uuid,
    error: &str,
) -> Result<Option<ForgejoPushReceipt>, sqlx::Error> {
    transition(
        db,
        sqlx::query(AssertSqlSafe(format!(
            "UPDATE {TABLE} SET status = 'failed', updated_at = now(), \
             failure_detail = $2 \
             WHERE id = $1 AND status IN ('pending', 'analyzed', 'failed') \
             RETURNING {SELECT}"
        )))
        .bind(receipt_id)
        .bind(error),
    )
    .await
}
