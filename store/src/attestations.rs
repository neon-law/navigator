//! `attestations` — the durable local record of one on-chain attorney
//! attestation (the Neon Law Node product), and every query against the
//! table.
//!
//! Commands use PostgreSQL whenever its host is configured; see
//! [storage models](../../docs/storage-models.md#matter-records).
//!
//! Written by the workflow worker inside the `onchain__record_attestation`
//! step's `ctx.run`. The row is the **system of record**: written
//! unconditionally, even when no chain backend is configured (`status`
//! stays `pending`, `chain` is `null`). A real Solana write later fills
//! `pda` / `tx_signature` and flips `status` to `recorded`.
//!
//! # One row per notation, by convention rather than by index
//!
//! The Surreal schema carries no unique index on `notation_id`
//! (`store/src/schema/navigator.surql`'s
//! comment on `attestation_notation` says so) — [`record`] reads the
//! existing row back before writing, the same shape
//! `crate::entities::create`'s firm-anchor guard uses. This is safe here
//! specifically because the only writer is a journaled `ctx.run` step keyed
//! on one notation at a time; it is not a general concurrency guarantee.

use crate::postgres::matter;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::application_store::ApplicationStore;

/// The table these rows live in.
const TABLE: &str = "attestation";

/// Attestation statuses. The row is [`STATUS_PENDING`] until a real chain
/// tx lands ([`STATUS_RECORDED`]); [`STATUS_FAILED`] records a chain write
/// that errored.
pub const STATUS_PENDING: &str = "pending";
pub const STATUS_RECORDED: &str = "recorded";
pub const STATUS_FAILED: &str = "failed";

/// One on-chain attestation record.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`AttestationRow`] is the seam that turns it into (and back out of) what
/// the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    pub id: Uuid,
    pub notation_id: Uuid,
    /// On-chain backend: `solana`, or `"null"` when none is configured.
    pub chain: String,
    /// Lowercase hex SHA-256 of the attested document bytes.
    pub sha256: String,
    /// `pending` / `recorded` / `failed` — see the `STATUS_*` constants.
    pub status: String,
    pub pda: Option<String>,
    pub tx_signature: Option<String>,
    pub firm_wallet: Option<String>,
    pub client_wallet: Option<String>,
    /// RFC 3339 timestamp the on-chain tx confirmed; `None` while pending.
    pub recorded_at: Option<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What to record for one attestation. The `sha256` (of the attested
/// document) and `chain` are always known; `pda` / `tx_signature` /
/// `recorded_at` are present only once a real chain backend records it.
#[derive(Debug, Clone)]
pub struct NewAttestation<'a> {
    pub notation_id: Uuid,
    pub chain: &'a str,
    pub sha256: &'a str,
    pub status: &'a str,
    pub pda: Option<&'a str>,
    pub tx_signature: Option<&'a str>,
    pub firm_wallet: Option<&'a str>,
    pub client_wallet: Option<&'a str>,
    pub recorded_at: Option<&'a str>,
}

/// Errors reading or writing an attestation.
#[derive(Debug, thiserror::Error)]
pub enum AttestationError {
    #[error(transparent)]
    Postgres(#[from] matter::MatterAuthorityError),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing an attestation returned no usable row")]
    WriteReturnedNothing,
}

/// The attestation recorded for a notation, if any.
///
/// # Errors
///
/// Propagates any database error.
pub async fn by_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<Attestation>, crate::postgres::matter::MatterAuthorityError> {
    let pg = matter::authority_for(TABLE).await?;
    matter::find(&pg, TABLE, json!({"notation_id": notation_id})).await
}

/// Upsert one `attestations` row keyed on `notation_id`, returning it.
///
/// Reads the existing row back first (see the module header for why there
/// is no unique index to conflict on): on a match, every chain-outcome
/// column is overwritten — so a `pending` row becomes `recorded` when the
/// chain write later lands, and a journaled replay is idempotent.
/// `inserted_at` is preserved across the upsert.
///
/// # Errors
///
/// [`AttestationError::Db`] if the read or write fails.
pub async fn record(
    _db: &ApplicationStore,
    new: &NewAttestation<'_>,
) -> Result<Attestation, AttestationError> {
    let pg = matter::authority_for(TABLE).await?;
    let existing: Option<Attestation> =
        matter::find(&pg, TABLE, json!({"notation_id": new.notation_id})).await?;
    let now = Utc::now();
    let row = Attestation {
        id: existing.as_ref().map_or_else(Uuid::new_v4, |row| row.id),
        notation_id: new.notation_id,
        chain: new.chain.to_owned(),
        sha256: new.sha256.to_owned(),
        status: new.status.to_owned(),
        pda: new.pda.map(str::to_owned),
        tx_signature: new.tx_signature.map(str::to_owned),
        firm_wallet: new.firm_wallet.map(str::to_owned),
        client_wallet: new.client_wallet.map(str::to_owned),
        recorded_at: new.recorded_at.map(str::to_owned),
        inserted_at: existing.map_or(now, |row| row.inserted_at),
        updated_at: now,
    };
    Ok(matter::upsert(&pg, TABLE, &row).await?)
}

#[cfg(test)]
mod tests {}
