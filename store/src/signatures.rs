//! `signatures` — one signature request/execution on a Notation's document,
//! correlated back from the provider by `(provider, provider_id)`, and
//! every query against the table.
//!
//! Configured deployments correlate requests and record execution in PostgreSQL.
//! The compatibility engine is used only by processes without a PostgreSQL host.
//!
//! A Notation's document is sent to an e-signature provider (DocuSign); the
//! provider issues an opaque request id (an envelope id). This row records
//! that request so the inbound completion webhook
//! (`portal::esignature_webhook`) can resolve a callback back to its
//! Notation by matching `(provider, provider_id)` — the pair is unique.
//! `signer_person_id`/`field` name who signs where (null until known);
//! `signed_at` is stamped when the provider reports completion, and `state`
//! records the closed Navigator terminal-state model.
//!
//! `state` is envelope-wide. `completed_signer_roles` is the per-signer
//! record beside it: the declared signer roles (`client`, `firm`, …, in the
//! template's `signers:` order) whose recipient has finished their own step
//! while the envelope is still live. It is what tells a client who has
//! signed a sequential envelope apart from one who has not.
//!
//! `provider` was a SeaORM `DeriveActiveEnum` over `TEXT`; on this engine it
//! is a plain stored string, and [`SignatureProvider`] is the closed set
//! this module enforces at the Rust boundary instead of a column type.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::application_store::ApplicationStore;

/// The e-signature (and, for [`crate::notarizations`], notarization)
/// provider that executed a request. A closed set keeps call sites and the
/// webhook from inventing provider strings; today the firm signs
/// exclusively through DocuSign.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureProvider {
    /// DocuSign — the production e-signature seam (`portal::signature`).
    DocuSign,
    /// Dropbox Sign — an alternative e-signature provider a Firm may choose.
    DropboxSign,
}

impl SignatureProvider {
    /// Every provider in the registry.
    pub const ALL: [Self; 2] = [Self::DocuSign, Self::DropboxSign];

    /// String form stored in the `provider` column and matched by the
    /// completion webhook.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DocuSign => "docusign",
            Self::DropboxSign => "dropbox_sign",
        }
    }

    /// The provider's own page for a request, so Navigator can link out to it.
    ///
    /// DocuSign opens the envelope. Dropbox Sign opens its request list, which takes no id here.
    #[must_use]
    pub fn deep_link(&self, provider_id: &str) -> String {
        match self {
            Self::DocuSign => format!("https://app.docusign.com/documents/details/{provider_id}"),
            Self::DropboxSign => "https://app.hellosign.com/home/manage".to_string(),
        }
    }

    /// A short label for the provider, as the dashboard prints it.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::DocuSign => "DocuSign",
            Self::DropboxSign => "Dropbox Sign",
        }
    }

    /// The provider a stored string names — `None` for a value outside
    /// the closed set. The webhook journal's replay sweep resolves its
    /// receipts' provider column through this, the same closed set the
    /// column's writers used.
    #[must_use]
    pub fn from_stored(value: &str) -> Option<Self> {
        match value {
            "docusign" => Some(Self::DocuSign),
            "dropbox_sign" => Some(Self::DropboxSign),
            _ => None,
        }
    }
}

/// The closed lifecycle of one signature request.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureState {
    /// The provider request exists but has not reached a terminal outcome.
    Requested,
    /// The provider confirmed that every signer executed the envelope.
    Completed,
    /// A signer declined the envelope.
    Declined,
    /// The envelope was voided before completion.
    Voided,
    /// The envelope expired before completion.
    Expired,
}

impl SignatureState {
    /// String form stored in the `state` column.
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Completed => "completed",
            Self::Declined => "declined",
            Self::Voided => "voided",
            Self::Expired => "expired",
        }
    }

    fn from_stored(value: &str) -> Option<Self> {
        Some(match value {
            "requested" => Self::Requested,
            "completed" => Self::Completed,
            "declined" => Self::Declined,
            "voided" => Self::Voided,
            "expired" => Self::Expired,
            _ => return None,
        })
    }
}

/// One signature request/execution.
///
/// The application-facing shape: plain Rust types, no engine handles.
/// [`SignatureRow`] is the seam that turns it into (and back out of) what
/// the SDK reads and writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Signature {
    pub id: Uuid,
    pub notation_id: Uuid,
    pub signer_person_id: Option<Uuid>,
    pub field: Option<String>,
    /// The provider's stored string form — see [`SignatureProvider::as_str`].
    pub provider: String,
    pub provider_id: String,
    pub state: SignatureState,
    pub signed_at: Option<String>,
    /// Declared signer roles whose recipient has completed their own step.
    /// Empty until the provider reports a recipient completion.
    pub completed_signer_roles: Vec<String>,
    pub inserted_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Signature {
    /// Whether the recipient holding `role` has completed their own step.
    #[must_use]
    pub fn signer_completed(&self, role: &str) -> bool {
        self.completed_signer_roles.iter().any(|r| r == role)
    }
}

use crate::postgres::signatures as authority;

/// Errors reading or writing a signature.
#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    /// PostgreSQL, under the signature authority, failed.
    #[error(transparent)]
    Postgres(#[from] authority::SignatureFamilyError),
    /// A provider timestamp the SurrealDB path would store verbatim does
    /// not parse as an RFC 3339 instant, which is all the PostgreSQL
    /// `timestamptz` column accepts.
    #[error("`{0}` is not an RFC 3339 timestamp")]
    InvalidTimestamp(String),
    /// A write reported success but returned no row, or returned one this
    /// module could not read back.
    #[error("writing a signature returned no usable row")]
    WriteReturnedNothing,
}

/// The row as PostgreSQL's `to_jsonb` serves it, under the
/// `postgres::signatures` authority. `signed_at` comes back as a real
/// instant and is re-rendered in the RFC 3339 form the SurrealDB path
/// stores verbatim.
#[derive(serde::Deserialize)]
struct PgSignatureRow {
    id: Uuid,
    notation_id: Uuid,
    signer_person_id: Option<Uuid>,
    field: Option<String>,
    provider: String,
    provider_id: String,
    state: String,
    signed_at: Option<DateTime<Utc>>,
    completed_signer_roles: Option<Vec<String>>,
    inserted_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<PgSignatureRow> for Signature {
    fn from(row: PgSignatureRow) -> Self {
        Signature {
            id: row.id,
            notation_id: row.notation_id,
            signer_person_id: row.signer_person_id,
            field: row.field,
            provider: row.provider,
            provider_id: row.provider_id,
            // A stored state outside the closed vocabulary is the Surreal
            // path's `None` from `into_signature`: the row is skipped.
            state: SignatureState::from_stored(&row.state).unwrap_or(SignatureState::Requested),
            signed_at: row
                .signed_at
                .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::AutoSi, true)),
            completed_signer_roles: row.completed_signer_roles.unwrap_or_default(),
            inserted_at: row.inserted_at,
            updated_at: row.updated_at,
        }
    }
}

/// The RFC 3339 instant the `timestamptz` column accepts for a provider
/// timestamp string.
fn instant(raw: &str) -> Result<DateTime<Utc>, SignatureError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| SignatureError::InvalidTimestamp(raw.to_string()))
}

/// Record the provider's request id for a Notation when the envelope is
/// created. Idempotent on `(provider, provider_id)`: re-recording the same
/// envelope returns the existing row rather than inserting a duplicate — a
/// concurrent double-send loses the `signature_provider_request` unique
/// index and re-reads the winner's row.
///
/// # Errors
///
/// [`SignatureError::Db`] if the write fails.
pub async fn record_request(
    db: &ApplicationStore,
    notation_id: Uuid,
    provider: SignatureProvider,
    provider_id: &str,
) -> Result<Signature, SignatureError> {
    let pg = authority::authority().await?;
    let row = serde_json::json!({
        "id": Uuid::now_v7(), "notation_id": notation_id,
        "provider": provider.as_str(), "provider_id": provider_id,
        "state": SignatureState::Requested.as_str(),
    });
    let query = sqlx::query(
        "INSERT INTO signature SELECT * FROM \
         jsonb_populate_record(NULL::signature, \
         $1 || jsonb_build_object('inserted_at', now(), 'updated_at', now())) \
         RETURNING to_jsonb(signature) AS row",
    )
    .bind(row);
    match authority::maybe::<PgSignatureRow>(&pg, query).await {
        Ok(Some(row)) => Ok(Signature::from(row)),
        Ok(None) => Err(SignatureError::WriteReturnedNothing),
        Err(error) if error.violates("signature_provider_request") => {
            by_provider(db, provider, provider_id)
                .await?
                .filter(|signature| signature.notation_id == notation_id)
                .ok_or(SignatureError::WriteReturnedNothing)
        }
        Err(error) => Err(error.into()),
    }
}

/// The signature row for `(provider, provider_id)`, if any — the webhook's
/// correlation lookup.
///
/// # Errors
///
/// [`SignatureError::Db`] if the lookup fails.
pub async fn by_provider(
    _db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
) -> Result<Option<Signature>, SignatureError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(signature) AS row FROM signature \
         WHERE provider = $1 AND provider_id = $2 LIMIT 1",
    )
    .bind(provider.as_str())
    .bind(provider_id);
    Ok(authority::maybe::<PgSignatureRow>(&pg, query)
        .await?
        .map(Signature::from))
}

/// The provider request id sent for a Notation, if one has been sent. The
/// notation-scoped read that replaces `notation.signature_request_id`;
/// returns the most recently recorded envelope for the notation.
///
/// # Errors
///
/// [`SignatureError::Db`] if the lookup fails.
pub async fn request_id_for_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<String>, SignatureError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT provider_id FROM signature WHERE notation_id = $1 \
         ORDER BY inserted_at DESC LIMIT 1",
    )
    .bind(notation_id);
    Ok(authority::maybe_scalar::<String>(&pg, query).await?)
}

/// The completed signature for a Notation, if the provider has ever
/// confirmed execution — a row with `signed_at` set. `None` whether no
/// envelope was ever sent, one is still outstanding, or one was declined
/// or voided: in every one of those cases there is no provider-confirmed
/// execution to report, which is exactly the distinction this query
/// exists to preserve for a caller (the matter page's "Signed" label)
/// that must not infer execution from anything else — an object's
/// presence in storage, a workflow state, or a row's mere existence.
///
/// # Errors
///
/// [`SignatureError::Db`] if the lookup fails.
pub async fn completed_for_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<Signature>, SignatureError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(signature) AS row FROM signature \
         WHERE notation_id = $1 AND signed_at IS NOT NULL \
         ORDER BY inserted_at DESC LIMIT 1",
    )
    .bind(notation_id);
    Ok(authority::maybe::<PgSignatureRow>(&pg, query)
        .await?
        .map(Signature::from))
}

/// The most recently recorded signature row for a Notation, in any state —
/// `None` when no envelope has ever been requested. Unlike
/// [`completed_for_notation`], which answers only "has this executed",
/// this reads the envelope's current state regardless of outcome, so a
/// caller can tell a live envelope (state
/// [`SignatureState::Requested`]) apart from one that never started, and
/// a declined, voided, or expired envelope from one still outstanding.
///
/// # Errors
///
/// [`SignatureError::Db`] if the lookup fails.
pub async fn latest_for_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Option<Signature>, SignatureError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(signature) AS row FROM signature \
         WHERE notation_id = $1 ORDER BY inserted_at DESC LIMIT 1",
    )
    .bind(notation_id);
    Ok(authority::maybe::<PgSignatureRow>(&pg, query)
        .await?
        .map(Signature::from))
}

/// Every signature request on a Notation, oldest first — one row per
/// envelope the firm sent, each with its own state and signer.
///
/// # Errors
///
/// [`SignatureError::Db`] if the lookup fails.
pub async fn for_notation(
    _db: &ApplicationStore,
    notation_id: Uuid,
) -> Result<Vec<Signature>, SignatureError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(signature) AS row FROM signature \
         WHERE notation_id = $1 ORDER BY inserted_at, id",
    )
    .bind(notation_id);
    Ok(authority::rows::<PgSignatureRow>(&pg, query)
        .await?
        .into_iter()
        .map(Signature::from)
        .collect())
}

/// Every provider-confirmed completed signature across a set of notations —
/// the batched resolver behind the matter-level onboarding-status check
/// (`store::projects::onboarding_official_sets`), which asks one question of
/// many notations at once: has any of these envelopes executed. Only rows
/// with `signed_at` set come back — a live, declined, voided, or expired
/// envelope is not execution, the same distinction
/// [`completed_for_notation`] draws for a single notation.
///
/// An empty `notation_ids` returns an empty vec without a query.
///
/// # Errors
///
/// [`SignatureError::Db`] if the lookup fails.
pub async fn completed_for_notations(
    _db: &ApplicationStore,
    notation_ids: &[Uuid],
) -> Result<Vec<Signature>, SignatureError> {
    if notation_ids.is_empty() {
        return Ok(Vec::new());
    }
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "SELECT to_jsonb(signature) AS row FROM signature \
         WHERE notation_id = ANY($1) AND signed_at IS NOT NULL",
    )
    .bind(notation_ids);
    Ok(authority::rows::<PgSignatureRow>(&pg, query)
        .await?
        .into_iter()
        .map(Signature::from)
        .collect())
}

/// Stamp `signed_at` on the signature for `(provider, provider_id)` when
/// the provider reports completion. A no-op (returns `false`) for an
/// unknown envelope — the callback may arrive for one we never tracked, or
/// twice.
///
/// # Errors
///
/// [`SignatureError::Db`] if the lookup or write fails.
pub async fn stamp_signed(
    db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
    signed_at: &str,
) -> Result<bool, SignatureError> {
    let pg = authority::authority().await?;
    let signed_at = instant(signed_at)?;
    let query = sqlx::query(
        "UPDATE signature SET state = 'completed', \
         signed_at = COALESCE(signed_at, $1), updated_at = now() \
         WHERE provider = $2 AND provider_id = $3 AND state = 'requested'",
    )
    .bind(signed_at)
    .bind(provider.as_str())
    .bind(provider_id);
    if authority::affected(&pg, query).await? > 0 {
        return Ok(true);
    }
    // The update found nothing to stamp: either the envelope is
    // unknown (`false`, like the SurrealDB path) or it already left
    // `requested` — where `completed` stays `true` and every other
    // terminal state stays `false`, the same distinction the SurrealDB
    // path draws from the row it re-reads.
    Ok(by_provider(db, provider, provider_id)
        .await?
        .is_some_and(|row| row.state == SignatureState::Completed))
}

/// Record that the recipient holding signer `role` completed their own step
/// on a live envelope. Idempotent: a role is recorded once. Returns `false`
/// for an unknown envelope or one already in a terminal state, so a late
/// recipient event can never rewrite a finished envelope.
///
/// # Errors
///
/// [`SignatureError::Db`] if the lookup or write fails.
pub async fn record_signer_completion(
    _db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
    role: &str,
) -> Result<bool, SignatureError> {
    let pg = authority::authority().await?;
    // Containment keeps the append idempotent — SurrealDB's
    // `array::union` records a role once — and the state guard means a
    // late recipient event can never rewrite a finished envelope.
    let query = sqlx::query(
        "UPDATE signature SET completed_signer_roles = \
         CASE WHEN completed_signer_roles @> to_jsonb(ARRAY[$1]) \
              THEN completed_signer_roles \
              ELSE COALESCE(completed_signer_roles, '[]'::jsonb) \
                   || to_jsonb(ARRAY[$1]) END, \
         updated_at = now() \
         WHERE provider = $2 AND provider_id = $3 AND state = 'requested'",
    )
    .bind(role)
    .bind(provider.as_str())
    .bind(provider_id);
    Ok(authority::affected(&pg, query).await? > 0)
}

async fn stamp_terminal(
    _db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
    state: SignatureState,
) -> Result<bool, SignatureError> {
    let pg = authority::authority().await?;
    let query = sqlx::query(
        "UPDATE signature SET state = $1, updated_at = now() \
         WHERE provider = $2 AND provider_id = $3 AND state = 'requested'",
    )
    .bind(state.as_str())
    .bind(provider.as_str())
    .bind(provider_id);
    Ok(authority::affected(&pg, query).await? > 0)
}

/// Stamp a declined terminal state for a known provider envelope.
///
/// A missing envelope or an already-terminal row is a no-op.
pub async fn stamp_declined(
    db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
) -> Result<bool, SignatureError> {
    stamp_terminal(db, provider, provider_id, SignatureState::Declined).await
}

/// Stamp a voided terminal state for a known provider envelope.
///
/// A missing envelope or an already-terminal row is a no-op.
pub async fn stamp_voided(
    db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
) -> Result<bool, SignatureError> {
    stamp_terminal(db, provider, provider_id, SignatureState::Voided).await
}

/// Stamp an expired terminal state for a known provider envelope.
///
/// A missing envelope or an already-terminal row is a no-op.
pub async fn stamp_expired(
    db: &ApplicationStore,
    provider: SignatureProvider,
    provider_id: &str,
) -> Result<bool, SignatureError> {
    stamp_terminal(db, provider, provider_id, SignatureState::Expired).await
}

#[cfg(test)]
mod tests {}
