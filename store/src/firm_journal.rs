//! Firm-owned immutable double-entry Journal Entries, exact asset quantities,
//! bounded source references, and auditable reconciliation evidence.
use crate::postgres::PostgresDb;
use crate::surreal::SurrealDb;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("invalid journal command: {0}")]
    Invalid(String),
    #[error("journal idempotency key already has different content")]
    Conflict,
    #[error("database: {0}")]
    Db(#[from] surrealdb::Error),
    #[error("PostgreSQL journal command failed")]
    Postgres(#[from] sqlx::Error),
    #[error("journal encoding: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("reconciliation evidence, audits, and signals require the PostgreSQL journal")]
    RequiresPostgres,
}

/// The environment variable whose presence selects the PostgreSQL journal.
pub const POSTGRES_HOST_ENV: &str = "NAVIGATOR_POSTGRES_HOST";

/// Where Journal Entries live. PostgreSQL when [`POSTGRES_HOST_ENV`] is set;
/// otherwise SurrealDB's `journal_entry` table, the same rows the PostgreSQL
/// startup import copies once PostgreSQL is configured. Both stores run the
/// same command checks; reconciliation evidence, audits, and signals exist
/// only in PostgreSQL.
#[derive(Debug, Clone)]
pub enum JournalStore {
    Postgres(PostgresDb),
    Surreal(SurrealDb),
}
impl JournalStore {
    /// The backend name for a deployment with or without a PostgreSQL host.
    #[must_use]
    pub fn selected_backend(postgres_configured: bool) -> &'static str {
        if postgres_configured {
            "postgresql"
        } else {
            "surrealdb"
        }
    }
    /// Whether this process is configured for the PostgreSQL journal.
    #[must_use]
    pub fn postgres_configured() -> bool {
        std::env::var_os(POSTGRES_HOST_ENV).is_some()
    }
    /// Select the store from the environment. A configured but unreachable
    /// PostgreSQL is an error, never a silent fall back to SurrealDB.
    pub async fn from_env(surreal: &SurrealDb) -> Result<Self, crate::postgres::PostgresError> {
        if Self::postgres_configured() {
            Ok(Self::Postgres(PostgresDb::from_env().await?))
        } else {
            Ok(Self::Surreal(surreal.clone()))
        }
    }
    #[must_use]
    pub fn backend(&self) -> &'static str {
        Self::selected_backend(matches!(self, Self::Postgres(_)))
    }
    /// The PostgreSQL journal, for commands that exist only there.
    pub fn postgres(&self) -> Result<&PostgresDb, JournalError> {
        match self {
            Self::Postgres(pg) => Ok(pg),
            Self::Surreal(_) => Err(JournalError::RequiresPostgres),
        }
    }
    /// The Firm's Journal Entries in insertion order.
    pub async fn entries(&self, firm: Uuid) -> Result<Vec<JournalEntry>, JournalError> {
        match self {
            Self::Postgres(pg) => entries(pg, firm).await,
            Self::Surreal(surreal) => surreal_journal::entries(surreal, firm).await,
        }
    }
    /// See [`post`]. Retrying an identical key returns its identity.
    pub async fn post(&self, db: &SurrealDb, entry: &JournalEntry) -> Result<Uuid, JournalError> {
        match self {
            Self::Postgres(pg) => post(db, pg, entry).await,
            Self::Surreal(surreal) => surreal_journal::execute(db, surreal, entry, true).await,
        }
    }
    /// See [`preview`]. Nothing is persisted.
    pub async fn preview(
        &self,
        db: &SurrealDb,
        entry: &JournalEntry,
    ) -> Result<Uuid, JournalError> {
        match self {
            Self::Postgres(pg) => preview(db, pg, entry).await,
            Self::Surreal(surreal) => surreal_journal::execute(db, surreal, entry, false).await,
        }
    }
}

/// SurrealDB `journal_entry` rows: the canonical serialized entry keyed by
/// Firm and command key, with an RFC 3339 insertion timestamp.
mod surreal_journal {
    use super::{
        check_command, entry_digest, validate_history, validate_posting_evidence,
        validate_repayment, JournalEntry, JournalError,
    };
    use crate::surreal::{record_id, SurrealDb};
    use serde::Deserialize;
    use surrealdb::types::SurrealValue;
    use uuid::Uuid;

    const TABLE: &str = "journal_entry";

    /// Serializes commands in this process, standing in for PostgreSQL's
    /// Firm row lock while history checks read before they write.
    static COMMANDS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[derive(Deserialize, SurrealValue)]
    struct Row {
        firm_id: String,
        command_key: String,
        payload: String,
        inserted_at: Option<String>,
    }

    pub(super) async fn entries(
        db: &SurrealDb,
        firm: Uuid,
    ) -> Result<Vec<JournalEntry>, JournalError> {
        let mut result = db
            .query("SELECT firm_id, command_key, payload, IF inserted_at != NONE THEN <string>inserted_at ELSE NONE END AS inserted_at FROM journal_entry WHERE firm_id = $firm")
            .bind(("firm", firm.to_string()))
            .await?
            .check()?;
        let rows: Vec<Row> = result.take(0)?;
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            let entry: JournalEntry = serde_json::from_str(&row.payload)?;
            if row.firm_id != entry.firm_id.to_string() || row.command_key != entry.key {
                return Err(JournalError::Conflict);
            }
            entries.push((row.inserted_at, entry));
        }
        entries.sort_by(|(a, x), (b, y)| a.cmp(b).then(x.id.cmp(&y.id)));
        Ok(entries.into_iter().map(|(_, entry)| entry).collect())
    }

    fn existing(all: &[JournalEntry], entry: &JournalEntry) -> Option<Result<Uuid, JournalError>> {
        let found = all.iter().find(|e| e.key == entry.key)?;
        Some(match (entry_digest(found), entry_digest(entry)) {
            (Ok(a), Ok(b)) if a == b => Ok(found.id),
            (Err(error), _) | (_, Err(error)) => Err(error),
            _ => Err(JournalError::Conflict),
        })
    }

    pub(super) async fn execute(
        db: &SurrealDb,
        journal: &SurrealDb,
        entry: &JournalEntry,
        commit: bool,
    ) -> Result<Uuid, JournalError> {
        check_command(db, entry).await?;
        let _guard = COMMANDS.lock().await;
        let all = entries(journal, entry.firm_id).await?;
        if let Some(result) = existing(&all, entry) {
            return result;
        }
        if all.iter().any(|e| e.id == entry.id) {
            return Err(JournalError::Conflict);
        }
        validate_posting_evidence(db, entry).await?;
        validate_repayment(entry, &all)?;
        validate_history(entry, &all)?;
        if !commit {
            return Ok(entry.id);
        }
        let inserted = journal
            .query("CREATE $id CONTENT {firm_id: $firm, command_key: $key, payload: $payload, inserted_at: $at}")
            .bind(("id", record_id(TABLE, entry.id)))
            .bind(("firm", entry.firm_id.to_string()))
            .bind(("key", entry.key.clone()))
            .bind(("payload", serde_json::to_string(entry)?))
            .bind((
                "at",
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
            ))
            .await
            .and_then(surrealdb::IndexedResults::check);
        match inserted {
            Ok(_) => Ok(entry.id),
            // Another replica took the key first: identical content is a retry.
            Err(error) => existing(&entries(journal, entry.firm_id).await?, entry)
                .unwrap_or(Err(JournalError::Db(error))),
        }
    }
}
fn invalid(message: &str) -> JournalError {
    JournalError::Invalid(message.into())
}

/// The unit is part of every quantity. Contract + network distinguish tokens
/// sharing a ticker; noncash assets carry their own custody identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Unit {
    Fiat {
        currency: String,
        decimals: u8,
    },
    Crypto {
        network: String,
        contract: String,
        decimals: u8,
    },
    Noncash {
        #[serde(alias = "asset_id")]
        property_id: Uuid,
        decimals: u8,
    },
}
impl Unit {
    #[must_use]
    pub fn fiat(currency: &str, decimals: u8) -> Self {
        Self::Fiat {
            currency: currency.into(),
            decimals,
        }
    }
    #[must_use]
    pub fn decimals(&self) -> u8 {
        match self {
            Self::Fiat { decimals, .. }
            | Self::Crypto { decimals, .. }
            | Self::Noncash { decimals, .. } => *decimals,
        }
    }
    fn validate(&self) -> Result<(), JournalError> {
        if self.decimals() > 38 {
            return Err(invalid("asset precision exceeds exact integer range"));
        }
        match self {
            Self::Fiat { currency, decimals } => {
                let expected = match currency.as_str() {
                    "USD" | "EUR" | "GBP" | "CAD" | "AUD" | "CHF" => 2,
                    "JPY" | "KRW" => 0,
                    "KWD" | "BHD" => 3,
                    _ => {
                        return Err(invalid(
                            "unsupported fiat precision; add verified currency metadata",
                        ))
                    }
                };
                if *decimals != expected {
                    return Err(invalid("fiat precision differs from currency metadata"));
                }
            }
            Self::Crypto {
                network, contract, ..
            } if network.trim().is_empty() || contract.trim().is_empty() => {
                return Err(invalid("crypto requires network and token/native identity"))
            }
            _ => {}
        }
        Ok(())
    }
}
/// Canonical signed integer, encoded as a string to survive JSON without loss.
pub fn quantity(value: &str) -> Result<i128, JournalError> {
    let n = value
        .parse::<i128>()
        .map_err(|_| invalid("quantity must be an exact bounded integer"))?;
    if n.to_string() != value {
        return Err(invalid("quantity is not canonical"));
    }
    Ok(n)
}
/// Parse a provider decimal without binary floating point or rounding.
pub fn parse_decimal(value: &str, decimals: u8) -> Result<String, JournalError> {
    if decimals > 38 {
        return Err(invalid("precision exceeds exact range"));
    }
    let negative = value.starts_with('-');
    let value = value.strip_prefix('-').unwrap_or(value);
    let mut parts = value.split('.');
    let whole = parts.next().unwrap_or_default();
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || whole.is_empty()
        || !whole.bytes().all(|b| b.is_ascii_digit())
        || !fraction.bytes().all(|b| b.is_ascii_digit())
        || fraction.len() > usize::from(decimals)
    {
        return Err(invalid(
            "decimal quantity has invalid syntax or excess precision",
        ));
    }
    let digits = format!(
        "{}{}{}",
        whole,
        fraction,
        "0".repeat(usize::from(decimals) - fraction.len())
    );
    let n = digits
        .parse::<i128>()
        .map_err(|_| invalid("decimal quantity overflow"))?;
    Ok(if negative {
        n.checked_neg()
            .ok_or_else(|| invalid("quantity overflow"))?
    } else {
        n
    }
    .to_string())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Cash,
    Trust,
    AdvancedCost,
    Reimbursement,
    EarnedFee,
    LawyerPayment,
    Recovery,
    ClientDisbursement,
    Award,
    NoncashReceipt,
    Custody,
    Disposal,
    Conversion,
    Fee,
    RealizedDifference,
}
/// Beneficial ownership is independent of asset type and account custody.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeneficialOwnership {
    Firm,
    Client,
    #[default]
    Unspecified,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Posting {
    pub account: String,
    #[serde(default)]
    pub ownership: BeneficialOwnership,
    #[serde(default)]
    pub restrictions: Vec<String>,
    pub unit: Unit,
    pub quantity: String,
    pub project_id: Option<Uuid>,
    pub category: Category,
}
/// Typed adapter metadata is versioned independently of journal postings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceFacts {
    Manual,
    Invoice {
        invoice_id: Uuid,
        number: Option<String>,
        total: String,
        paid: String,
        due: String,
        payment_ids: Vec<Uuid>,
    },
    CreditNote {
        credit_note_id: Uuid,
        number: Option<String>,
        total: String,
        remaining_credit: String,
        payment_ids: Vec<Uuid>,
    },
    Payment {
        #[serde(default)]
        credit_note_id: Option<Uuid>,
        payment_id: Uuid,
        invoice_id: Option<Uuid>,
        invoice_number: Option<String>,
        #[serde(alias = "invoice_currency")]
        original_currency: Option<String>,
        #[serde(alias = "invoice_quantity")]
        original_quantity: String,
        bank_quantity: String,
        currency_rate: Option<String>,
        reconciled: Option<bool>,
    },
    BankTransaction {
        transaction_id: Uuid,
        transaction_type: String,
        currency_rate: Option<String>,
        reconciled: Option<bool>,
    },
    BankTransfer {
        reconciled: Option<bool>,
        transfer_id: Uuid,
        from_account_id: Uuid,
        to_account_id: Uuid,
        from_transaction_id: Uuid,
        to_transaction_id: Uuid,
    },
    Crypto {
        transaction_hash: String,
        transfer_locator: String,
        wallet: String,
        finality_evidence_document_id: Uuid,
        #[serde(default)]
        finality: CryptoFinality,
        fee_unit: Unit,
        fee_quantity: String,
    },
    NetworkFee {
        transaction_hash: String,
        wallet: String,
        finality_evidence_document_id: Uuid,
        #[serde(default)]
        finality: CryptoFinality,
    },
    Noncash {
        #[serde(alias = "asset_id")]
        property_id: Uuid,
        description: String,
        custody_reference: String,
        received: bool,
        valuation: Valuation,
    },
}

/// Reviewed network finality requirement, independent of provider success status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "policy", rename_all = "snake_case", deny_unknown_fields)]
pub enum CryptoFinality {
    #[default]
    Unknown,
    Finalized,
    Confirmations {
        observed: u64,
        required: u64,
    },
}
impl CryptoFinality {
    #[must_use]
    pub fn satisfied(&self) -> bool {
        match self {
            Self::Unknown => false,
            Self::Finalized => true,
            Self::Confirmations { observed, required } => *required > 0 && observed >= required,
        }
    }
}

/// External identifiers identify observations, never authorize rewriting a posting.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReference {
    pub provider: String,
    pub scope: String,
    pub account: String,
    pub object_type: String,
    pub external_id: String,
    pub locator: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkKind {
    Transfer,
    Repayment,
    InvoicePayment,
    Refund,
    Conversion,
    Disposal,
    Engagement,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalLink {
    pub kind: LinkKind,
    pub target_id: Uuid,
    pub verified_by: Uuid,
    pub verified_at: String,
    pub evidence_document_id: Uuid,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Valuation {
    Unknown,
    Known {
        reporting_unit: Unit,
        quantity: String,
        at: String,
        /// Exact reporting units per original asset unit, never a floating-point number.
        rate: String,
        rate_source: String,
        evidence_document_id: Uuid,
    },
}
impl Valuation {
    #[must_use]
    pub fn reporting_quantity(&self) -> Option<&str> {
        match self {
            Self::Unknown => None,
            Self::Known { quantity, .. } => Some(quantity),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalEntry {
    pub id: Uuid,
    pub firm_id: Uuid,
    pub key: String,
    pub actor_id: Uuid,
    pub occurred_at: String,
    pub memo: String,
    pub postings: Vec<Posting>,
    pub sources: Vec<SourceReference>,
    pub receipts: Vec<Uuid>,
    pub links: Vec<JournalLink>,
    pub reversal_of: Option<Uuid>,
}
impl JournalEntry {
    pub fn validate(&self) -> Result<(), JournalError> {
        if self.key.trim().is_empty()
            || self.key.len() > 200
            || self.memo.trim().is_empty()
            || self.postings.len() < 2
            || self.postings.len() > 1000
        {
            return Err(invalid("entry requires key, memo and bounded postings"));
        }
        chrono::DateTime::parse_from_rfc3339(&self.occurred_at)
            .map_err(|_| invalid("invalid occurrence timestamp"))?;
        let mut balances = BTreeMap::<Unit, i128>::new();
        for p in &self.postings {
            p.unit.validate()?;
            let n = quantity(&p.quantity)?;
            if n == 0 || p.account.trim().is_empty() {
                return Err(invalid("posting requires account and nonzero quantity"));
            }
            if matches!(p.category, Category::Trust) && !matches!(p.unit, Unit::Fiat { .. }) {
                return Err(invalid(
                    "nonfiat assets require explicit custody; they are not IOLTA funds",
                ));
            }
            if matches!(p.category, Category::AdvancedCost | Category::Reimbursement)
                && (p.project_id.is_none()
                    || !self.links.iter().any(|l| l.kind == LinkKind::Engagement))
            {
                return Err(invalid("client costs require Project allocation and verified signed engagement evidence"));
            }
            if p.category == Category::Reimbursement
                && !self.links.iter().any(|l| l.kind == LinkKind::Repayment)
            {
                return Err(invalid("reimbursement requires original cost link"));
            }
            let sum = balances.entry(p.unit.clone()).or_default();
            *sum = sum
                .checked_add(n)
                .ok_or_else(|| invalid("balance overflow"))?;
        }
        if balances.values().any(|n| *n != 0) {
            return Err(invalid(
                "postings must balance separately in each asset unit",
            ));
        }
        let mut unique = BTreeSet::new();
        for s in &self.sources {
            if [
                &s.provider,
                &s.scope,
                &s.account,
                &s.object_type,
                &s.external_id,
            ]
            .iter()
            .any(|s| s.trim().is_empty())
                || !unique.insert(s)
            {
                return Err(invalid("source references must be scoped and unique"));
            }
        }
        for l in &self.links {
            chrono::DateTime::parse_from_rfc3339(&l.verified_at)
                .map_err(|_| invalid("invalid link verification timestamp"))?;
        }
        Ok(())
    }
    pub fn reversal(&self, key: &str, actor_id: Uuid, at: &str) -> Result<Self, JournalError> {
        let mut result = self.clone();
        result.id = Uuid::new_v4();
        result.key = key.into();
        result.actor_id = actor_id;
        result.occurred_at = at.into();
        result.reversal_of = Some(self.id);
        result.sources.clear();
        for p in &mut result.postings {
            p.quantity = quantity(&p.quantity)?
                .checked_neg()
                .ok_or_else(|| invalid("reversal overflow"))?
                .to_string();
        }
        result.validate()?;
        Ok(result)
    }
}
fn digest(bytes: &[u8]) -> String {
    crate::documents::sha256_hex(bytes)
}
/// Atomic journal command under the owning Firm's PostgreSQL row lock.
/// A retry after commit returns the original identity; altered content conflicts.
pub async fn post(
    db: &SurrealDb,
    pg: &PostgresDb,
    entry: &JournalEntry,
) -> Result<Uuid, JournalError> {
    execute_post(db, pg, entry, true).await
}
/// Run the same evidence, tenancy, allocation and database checks without persisting a command.
pub async fn preview(
    db: &SurrealDb,
    pg: &PostgresDb,
    entry: &JournalEntry,
) -> Result<Uuid, JournalError> {
    execute_post(db, pg, entry, false).await
}
/// Store-independent command checks: shape, balance, ownership, and the owning Firm.
async fn check_command(
    db: &SurrealDb,
    entry: &JournalEntry,
) -> Result<crate::firms::Firm, JournalError> {
    entry.validate()?;
    if entry.postings.iter().any(|posting| {
        posting.ownership == BeneficialOwnership::Unspecified
            || posting.restrictions.len() > 32
            || posting
                .restrictions
                .iter()
                .any(|r| r.trim().is_empty() || r.len() > 256)
            || (posting.category == Category::Trust
                && posting.ownership != BeneficialOwnership::Client)
    }) {
        return Err(invalid(
            "posting requires explicit beneficial ownership and bounded restrictions",
        ));
    }
    crate::firms::find_by_id(db, entry.firm_id)
        .await
        .map_err(|e| invalid(&e.to_string()))?
        .ok_or_else(|| invalid("Firm missing"))
}
/// Hold the Firm's id and immutable slug before a journal row names it.
/// Concurrent admissions may conflict on either unique index. A conflicting
/// insert is safe only when a separate read confirms the same id and slug;
/// existing names, status, and timestamps are left alone.
pub(crate) async fn ensure_firm_row(
    connection: &mut sqlx::PgConnection,
    firm: &crate::firms::Firm,
) -> Result<(), JournalError> {
    sqlx::query("INSERT INTO firm(id,name,slug,status,inserted_at,updated_at) VALUES($1,$2,$3,$4,$5::timestamptz,$6::timestamptz) ON CONFLICT DO NOTHING")
        .bind(firm.id)
        .bind(&firm.name)
        .bind(&firm.slug)
        .bind(&firm.status)
        .bind(&firm.inserted_at)
        .bind(&firm.updated_at)
        .execute(&mut *connection)
        .await?;
    // This statement sees the committed row after a concurrent insert wins.
    let slug: Option<String> = sqlx::query_scalar("SELECT slug FROM firm WHERE id=$1")
        .bind(firm.id)
        .fetch_optional(connection)
        .await?;
    if slug.as_deref() != Some(firm.slug.as_str()) {
        return Err(invalid("Firm identity and immutable slug conflict"));
    }
    Ok(())
}
async fn execute_post(
    db: &SurrealDb,
    pg: &PostgresDb,
    entry: &JournalEntry,
    commit: bool,
) -> Result<Uuid, JournalError> {
    let firm = check_command(db, entry).await?;
    let payload = serde_json::to_value(entry)?;
    let hash = entry_digest(entry)?;
    let mut tx = pg.0.begin().await?;
    ensure_firm_row(&mut tx, &firm).await?;
    sqlx::query("SELECT id FROM firm WHERE id=$1 FOR UPDATE")
        .bind(entry.firm_id)
        .fetch_one(&mut *tx)
        .await?;
    let existing: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id,digest FROM journal_entry WHERE firm_id=$1 AND command_key=$2")
            .bind(entry.firm_id)
            .bind(&entry.key)
            .fetch_optional(&mut *tx)
            .await?;
    if let Some((id, digest)) = existing {
        return if digest == hash {
            Ok(id)
        } else {
            Err(JournalError::Conflict)
        };
    }
    let rows: Vec<(sqlx::types::Json<JournalEntry>,)> = sqlx::query_as(
        "SELECT payload FROM journal_entry WHERE firm_id=$1 ORDER BY inserted_at,id",
    )
    .bind(entry.firm_id)
    .fetch_all(&mut *tx)
    .await?;
    let all: Vec<_> = rows.into_iter().map(|(e,)| e.0).collect();
    validate_posting_evidence(db, entry).await?;
    validate_repayment(entry, &all)?;
    validate_history(entry, &all)?;

    sqlx::query("INSERT INTO journal_entry(id,firm_id,command_key,actor_id,occurred_at,memo,payload,digest,reversal_of) VALUES($1,$2,$3,$4,$5::timestamptz,$6,$7,$8,$9)")
        .bind(entry.id).bind(entry.firm_id).bind(&entry.key).bind(entry.actor_id).bind(&entry.occurred_at).bind(&entry.memo).bind(sqlx::types::Json(payload)).bind(hash).bind(entry.reversal_of).execute(&mut *tx).await?;
    for (position, p) in entry.postings.iter().enumerate() {
        sqlx::query("INSERT INTO journal_posting(entry_id,firm_id,position,account,unit,quantity,project_id,category,ownership,restrictions) VALUES($1,$2,$3,$4,$5,$6::numeric,$7,$8,$9,$10)").bind(entry.id).bind(entry.firm_id).bind(i32::try_from(position).map_err(|_|invalid("posting limit"))?).bind(&p.account).bind(sqlx::types::Json(&p.unit)).bind(&p.quantity).bind(p.project_id).bind(serde_json::to_value(p.category)?.as_str().ok_or_else(||invalid("category encoding"))?).bind(serde_json::to_value(p.ownership)?.as_str().ok_or_else(||invalid("ownership encoding"))?).bind(sqlx::types::Json(&p.restrictions)).execute(&mut *tx).await?;
    }
    for source in &entry.sources {
        let id = persist_source(&mut tx, entry.firm_id, source).await?;
        sqlx::query("INSERT INTO journal_entry_source(entry_id,source_id) VALUES($1,$2)")
            .bind(entry.id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    if commit {
        tx.commit().await?;
    } else {
        sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
            .execute(&mut *tx)
            .await?;
        tx.rollback().await?;
    }
    Ok(entry.id)
}

async fn validate_posting_evidence(
    db: &SurrealDb,
    entry: &JournalEntry,
) -> Result<(), JournalError> {
    for link in &entry.links {
        let verifier = crate::persons::find_by_id(db, link.verified_by)
            .await
            .map_err(|_| invalid("link verifier lookup failed"))?
            .ok_or_else(|| invalid("link verifier is missing"))?;
        if !matches!(
            verifier.role,
            crate::persons::Role::Owner
                | crate::persons::Role::Admin
                | crate::persons::Role::Lawyer
        ) || !crate::firm_capability::FirmCapability::ViewDirectory
            .resolve(db, verifier.role, Some(verifier.id), entry.firm_id)
            .await
            .map_err(|_| invalid("link verifier Firm scope unavailable"))?
            .is_allowed()
        {
            return Err(invalid("link requires verified Firm-side review"));
        }
        if chrono::DateTime::parse_from_rfc3339(&link.verified_at)
            .map_err(|_| invalid("invalid verification timestamp"))?
            > chrono::Utc::now()
        {
            return Err(invalid("link verification cannot be in the future"));
        }
    }
    let mut signed_projects = BTreeSet::new();
    let mut documents = entry.receipts.clone();
    documents.extend(entry.links.iter().map(|l| l.evidence_document_id));
    for id in documents {
        let asset = evidence_firm(db, id, entry.firm_id).await?;
        let project = asset
            .project_id
            .ok_or_else(|| invalid("journal evidence requires Project"))?;
        if entry
            .links
            .iter()
            .any(|l| l.kind == LinkKind::Engagement && l.evidence_document_id == id)
        {
            let signing = asset.signing.ok_or_else(|| {
                invalid("client reimbursement requires signed engagement evidence")
            })?;
            if signing.signed_on > chrono::Utc::now().date_naive() {
                return Err(invalid("engagement signing date cannot be in the future"));
            }
            signed_projects.insert(project);
        }
    }
    for p in &entry.postings {
        if let Some(id) = p.project_id {
            if matches!(p.category, Category::AdvancedCost | Category::Reimbursement)
                && !signed_projects.contains(&id)
            {
                return Err(invalid(
                    "cost allocation lacks that Project's verified signed engagement",
                ));
            }
            let project = crate::projects::find_by_id(db, id)
                .await
                .map_err(|e| JournalError::Invalid(e.to_string()))?
                .ok_or_else(|| invalid("Project missing"))?;
            if project.firm_id != Some(entry.firm_id) {
                return Err(invalid("Project belongs to another Firm"));
            }
        }
    }
    Ok(())
}
fn validate_repayment(entry: &JournalEntry, all: &[JournalEntry]) -> Result<(), JournalError> {
    if entry
        .postings
        .iter()
        .any(|p| p.category == Category::Reimbursement)
        && entry.reversal_of.is_none()
    {
        let costs: Vec<_> = entry
            .links
            .iter()
            .filter(|l| l.kind == LinkKind::Repayment)
            .collect();
        if costs.len() != 1 {
            return Err(invalid(
                "one repayment command must identify exactly one cost entry",
            ));
        }
        let mut repayment_allocations = BTreeSet::new();
        for posting in entry
            .postings
            .iter()
            .filter(|p| p.category == Category::Reimbursement)
        {
            if !repayment_allocations.insert((posting.project_id, posting.unit.clone())) {
                return Err(invalid(
                    "combine reimbursement quantities for each Project and unit",
                ));
            }
        }
        let all = &all;
        let cost = all
            .iter()
            .find(|e| e.id == costs[0].target_id)
            .ok_or_else(|| invalid("original cost entry missing"))?;
        if all.iter().any(|entry| entry.reversal_of == Some(cost.id)) {
            return Err(invalid("reimbursement original cost has been reversed"));
        }
        for p in entry
            .postings
            .iter()
            .filter(|p| p.category == Category::Reimbursement)
        {
            let incurred = cost
                .postings
                .iter()
                .filter(|c| {
                    c.category == Category::AdvancedCost
                        && c.unit == p.unit
                        && c.project_id == p.project_id
                })
                .try_fold(0i128, |a, c| {
                    quantity(&c.quantity).ok().and_then(|n| a.checked_add(n))
                })
                .ok_or_else(|| invalid("cost overflow"))?;
            let repaid = all
                .iter()
                .filter(|e| {
                    e.id != entry.id
                        && e.links
                            .iter()
                            .any(|l| l.kind == LinkKind::Repayment && l.target_id == cost.id)
                })
                .flat_map(|e| &e.postings)
                .filter(|c| {
                    c.category == Category::Reimbursement
                        && c.unit == p.unit
                        && c.project_id == p.project_id
                })
                .try_fold(0i128, |a, c| {
                    quantity(&c.quantity).ok().and_then(|n| a.checked_add(n))
                })
                .ok_or_else(|| invalid("repayment overflow"))?;
            let n = quantity(&p.quantity)?;
            if n >= 0
                || incurred <= 0
                || incurred
                    .checked_add(repaid)
                    .and_then(|v| v.checked_add(n))
                    .is_none_or(|v| v < 0)
            {
                return Err(invalid("repayment exceeds incurred outstanding cost"));
            }
        }
    }
    Ok(())
}
fn validate_history(entry: &JournalEntry, all: &[JournalEntry]) -> Result<(), JournalError> {
    if entry.reversal_of.is_none() {
        for posting in entry
            .postings
            .iter()
            .filter(|p| p.category == Category::Fee)
        {
            if let Unit::Crypto { network, .. } = &posting.unit {
                if quantity(&posting.quantity)? < 0
                    && entry
                        .sources
                        .iter()
                        .any(|s| s.provider == "solana" && s.scope == *network)
                    && !entry.sources.iter().any(|fee| {
                        fee.provider == "solana"
                            && fee.scope == *network
                            && fee.object_type == "network_fee"
                            && fee.locator.is_none()
                    })
                {
                    return Err(invalid(
                        "chain network fee requires its unique transaction fee source",
                    ));
                }
            }
        }
    }
    if !entry.sources.is_empty() && entry.reversal_of.is_none() {
        let all = &all;
        for source in entry.sources.iter().filter(|s| {
            matches!(
                s.object_type.as_str(),
                "transaction"
                    | "transfer"
                    | "network_fee"
                    | "bank_transaction"
                    | "payment"
                    | "bank_transfer"
            )
        }) {
            if all.iter().any(|old| {
                old.key != entry.key
                    && old.sources.contains(source)
                    && !all.iter().any(|r| r.reversal_of == Some(old.id))
            }) {
                return Err(invalid("source movement already has an active journal event; allocate within that event"));
            }
        }
    }
    if let Some(original) = entry.reversal_of {
        let all = &all;
        let original = all
            .iter()
            .find(|e| e.id == original)
            .ok_or_else(|| invalid("reversal target missing in Firm"))?;
        if original.postings.len() != entry.postings.len()
            || original.postings.iter().zip(&entry.postings).any(|(a, b)| {
                a.account != b.account
                    || a.ownership != b.ownership
                    || a.restrictions != b.restrictions
                    || a.category != b.category
                    || a.unit != b.unit
                    || a.project_id != b.project_id
                    || quantity(&a.quantity).ok().and_then(i128::checked_neg)
                        != quantity(&b.quantity).ok()
            })
        {
            return Err(invalid("reversal must negate original postings exactly"));
        }
        if all.iter().any(|e| e.reversal_of == Some(original.id)) {
            return Err(invalid("entry already reversed"));
        }
    }
    Ok(())
}

pub async fn entries(pg: &PostgresDb, firm: Uuid) -> Result<Vec<JournalEntry>, JournalError> {
    let rows: Vec<(sqlx::types::Json<JournalEntry>,)> = sqlx::query_as(
        "SELECT payload FROM journal_entry WHERE firm_id=$1 ORDER BY inserted_at,id",
    )
    .bind(firm)
    .fetch_all(&pg.0)
    .await?;
    Ok(rows.into_iter().map(|(e,)| e.0).collect())
}
pub(crate) async fn persist_source(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    firm: Uuid,
    s: &SourceReference,
) -> Result<String, JournalError> {
    let id = digest(format!("{firm}:{}", serde_json::to_string(s)?).as_bytes());
    sqlx::query("INSERT INTO journal_source_reference(id,firm_id,provider,provider_scope,account,object_type,external_id,locator,payload) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(id) DO NOTHING").bind(&id).bind(firm).bind(&s.provider).bind(&s.scope).bind(&s.account).bind(&s.object_type).bind(&s.external_id).bind(&s.locator).bind(sqlx::types::Json(s)).execute(&mut **tx).await?;
    Ok(id)
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    Pending,
    Completed,
    Reversed,
    Failed,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceSnapshot {
    /// Provider occurrence timestamp, separate from evidence retrieval time.
    #[serde(default)]
    pub occurred_at: Option<String>,
    /// Reviewed Navigator account identity; provider account IDs remain in `source`.
    #[serde(default)]
    pub journal_account: Option<String>,
    pub adapter_version: String,
    pub facts: SourceFacts,
    pub source: SourceReference,
    pub revision: String,
    pub retrieved_at: String,
    pub digest: String,
    pub unit: Unit,
    pub quantity: String,
    pub status: SourceStatus,
    pub receipt_document_ids: Vec<Uuid>,
    #[serde(default)]
    pub evidence_document_ids: Vec<Uuid>,
    pub valuation: Valuation,
}
impl SourceSnapshot {
    #[must_use]
    pub fn synthetic(currency: &str, decimals: u8, quantity: &str) -> Self {
        Self {
            occurred_at: Some("2026-10-11T00:00:00Z".into()),
            journal_account: None,
            adapter_version: "manual-v1".into(),
            facts: SourceFacts::Manual,
            source: SourceReference {
                provider: "manual".into(),
                scope: "synthetic".into(),
                account: "bank".into(),
                object_type: "transaction".into(),
                external_id: "one".into(),
                locator: None,
            },
            revision: "1".into(),
            retrieved_at: "2026-10-11T00:00:00Z".into(),
            digest: digest(b"synthetic"),
            unit: Unit::fiat(currency, decimals),
            quantity: quantity.into(),
            status: SourceStatus::Completed,
            receipt_document_ids: vec![],
            evidence_document_ids: vec![],
            valuation: Valuation::Unknown,
        }
    }
}
/// Financial revision excludes transient response clocks and signed receipt URLs.
pub fn source_revision(snapshot: &SourceSnapshot) -> Result<String, JournalError> {
    Ok(digest(&serde_json::to_vec(&(
        snapshot.adapter_version.as_str(),
        &snapshot.source,
        &snapshot.facts,
        &snapshot.unit,
        snapshot.quantity.as_str(),
        snapshot.status,
        &snapshot.occurred_at,
    ))?))
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reconciliation {
    pub source: SourceReference,
    pub expected_quantity: String,
    pub actual_quantity: Option<String>,
    pub unit: Unit,
    pub checks: BTreeMap<String, bool>,
    pub exceptions: Vec<String>,
}
impl Reconciliation {
    #[must_use]
    pub fn reconciled(&self) -> bool {
        !self.checks.is_empty() && self.checks.values().all(|v| *v) && self.exceptions.is_empty()
    }
}
/// Compare one explicit source object with the immutable bank/custody side.
/// Missing evidence is an exception, never a fabricated zero-valued receipt.
#[must_use]
#[expect(
    clippy::too_many_lines,
    reason = "all required reconciliation checks are assembled in one result"
)]
pub fn compare(
    entry: &JournalEntry,
    snapshot: &SourceSnapshot,
    coverage_complete: bool,
) -> Reconciliation {
    let mut checks = BTreeMap::<String, bool>::new();
    checks.insert("balanced".into(), entry.validate().is_ok());
    checks.insert(
        "finality".into(),
        matches!(
            snapshot.status,
            SourceStatus::Completed | SourceStatus::Reversed
        ),
    );
    checks.insert("coverage".into(), coverage_complete);
    checks.insert(
        "adapter_version".into(),
        !snapshot.adapter_version.trim().is_empty(),
    );
    checks.insert(
        "provider_reconciliation".into(),
        match &snapshot.facts {
            SourceFacts::Payment { reconciled, .. }
            | SourceFacts::BankTransaction { reconciled, .. }
            | SourceFacts::BankTransfer { reconciled, .. } => *reconciled == Some(true),
            _ => true,
        },
    );
    checks.insert(
        "custody_or_finality".into(),
        match &snapshot.facts {
            SourceFacts::Noncash { received, .. } => *received,
            SourceFacts::Crypto {
                transfer_locator,
                transaction_hash,
                wallet,
                finality,
                ..
            } => {
                finality.satisfied()
                    && snapshot.source.locator.as_deref() == Some(transfer_locator.as_str())
                    && snapshot.source.external_id == *transaction_hash
                    && snapshot.source.account == *wallet
                    && matches!(&snapshot.unit, Unit::Crypto { network, .. } if network == &snapshot.source.scope)
            }
            SourceFacts::NetworkFee { transaction_hash, wallet, finality, .. } => {
                finality.satisfied()
                    && snapshot.source.object_type == "network_fee"
                    && snapshot.source.locator.is_none()
                    && snapshot.source.external_id == *transaction_hash
                    && snapshot.source.account == *wallet
                    && matches!(&snapshot.unit, Unit::Crypto { network, .. } if network == &snapshot.source.scope)
            }
            _ => true,
        },
    );
    checks.insert(
        "typed_source_identity".into(),
        match &snapshot.facts {
            SourceFacts::Invoice {
                invoice_id,
                total,
                due,
                ..
            } => {
                snapshot.source.object_type == "invoice"
                    && snapshot.source.external_id == invoice_id.to_string()
                    && due == &snapshot.quantity
                    && quantity(due)
                        .ok()
                        .zip(quantity(total).ok())
                        .is_some_and(|(due, total)| due >= 0 && due <= total)
            }
            SourceFacts::CreditNote {
                credit_note_id,
                total,
                remaining_credit,
                ..
            } => {
                snapshot.source.object_type == "credit_note"
                    && snapshot.source.external_id == credit_note_id.to_string()
                    && remaining_credit == &snapshot.quantity
                    && quantity(remaining_credit)
                        .ok()
                        .zip(quantity(total).ok())
                        .is_some_and(|(remaining, total)| remaining >= 0 && remaining <= total)
            }
            SourceFacts::Payment {
                payment_id,
                invoice_id,
                credit_note_id,
                bank_quantity,
                ..
            } => {
                snapshot.source.object_type == "payment"
                    && snapshot.source.external_id == payment_id.to_string()
                    && !(invoice_id.is_some() && credit_note_id.is_some())
                    && bank_quantity == &snapshot.quantity
            }
            _ => true,
        },
    );
    checks.insert(
        "source_identity".into(),
        entry.sources.is_empty() || entry.sources.contains(&snapshot.source),
    );
    let total = entry
        .postings
        .iter()
        .filter(|p| {
            p.unit == snapshot.unit
                && match snapshot.facts {
                    SourceFacts::Crypto { .. } => p.category != Category::Fee,
                    SourceFacts::NetworkFee { .. } => p.category == Category::Fee,
                    _ => true,
                }
                && p.account
                    == snapshot
                        .journal_account
                        .as_deref()
                        .unwrap_or(&snapshot.source.account)
        })
        .try_fold(0i128, |a, p| {
            quantity(&p.quantity).ok().and_then(|b| a.checked_add(b))
        });
    checks.insert(
        "quantity".into(),
        total.is_some()
            && total
                == if snapshot.status == SourceStatus::Reversed {
                    Some(0)
                } else {
                    quantity(&snapshot.quantity).ok()
                },
    );
    checks.insert(
        "receipts".into(),
        snapshot
            .receipt_document_ids
            .iter()
            .all(|id| entry.receipts.contains(id)),
    );
    let exceptions = checks
        .iter()
        .filter(|(_, ok)| !**ok)
        .map(|(name, _)| name.clone())
        .collect();
    Reconciliation {
        source: snapshot.source.clone(),
        expected_quantity: if snapshot.status == SourceStatus::Reversed {
            "0".into()
        } else {
            snapshot.quantity.clone()
        },
        actual_quantity: total.map(|n| n.to_string()),
        unit: snapshot.unit.clone(),
        checks,
        exceptions,
    }
}

/// An exact per-unit statement; unlike assets are never added together.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatementLine {
    pub account: String,
    pub ownership: BeneficialOwnership,
    pub restrictions: Vec<String>,
    pub unit: Unit,
    pub category: Category,
    pub quantity: String,
}
pub fn statement(
    entries: &[JournalEntry],
    project: Option<Uuid>,
) -> Result<Vec<StatementLine>, JournalError> {
    let mut sums =
        BTreeMap::<(String, Unit, String, BeneficialOwnership, Vec<String>), i128>::new();
    let mut categories = BTreeMap::new();
    for e in entries {
        for p in &e.postings {
            if project.is_some() && p.project_id != project {
                continue;
            }
            let category = serde_json::to_string(&p.category)?;
            let mut restrictions = p.restrictions.clone();
            restrictions.sort();
            restrictions.dedup();
            let key = (
                p.account.clone(),
                p.unit.clone(),
                category,
                p.ownership,
                restrictions,
            );
            categories.insert(key.clone(), p.category);
            let total = sums.entry(key).or_default();
            *total = total
                .checked_add(quantity(&p.quantity)?)
                .ok_or_else(|| invalid("statement overflow"))?;
        }
    }
    Ok(sums
        .into_iter()
        .map(|(key, n)| StatementLine {
            account: key.0.clone(),
            ownership: key.3,
            restrictions: key.4.clone(),
            unit: key.1.clone(),
            category: categories[&key],
            quantity: n.to_string(),
        })
        .collect())
}
/// Cost balances retain their own quantities independently of earned fees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostPosition {
    pub unit: Unit,
    pub incurred: String,
    pub reimbursed: String,
    pub outstanding: String,
}
pub fn cost_positions(
    entries: &[JournalEntry],
    project: Option<Uuid>,
) -> Result<Vec<CostPosition>, JournalError> {
    let mut positions = BTreeMap::<Unit, (i128, i128)>::new();
    for entry in entries {
        for posting in &entry.postings {
            if project.is_some() && posting.project_id != project {
                continue;
            }
            if !matches!(
                posting.category,
                Category::AdvancedCost | Category::Reimbursement
            ) {
                continue;
            }
            let position = positions.entry(posting.unit.clone()).or_default();
            let value = quantity(&posting.quantity)?;
            if posting.category == Category::AdvancedCost {
                position.0 = position
                    .0
                    .checked_add(value)
                    .ok_or_else(|| invalid("incurred cost overflow"))?;
            } else {
                position.1 = position
                    .1
                    .checked_sub(value)
                    .ok_or_else(|| invalid("reimbursement overflow"))?;
            }
        }
    }
    positions
        .into_iter()
        .map(|(unit, (incurred, reimbursed))| {
            let outstanding = incurred
                .checked_sub(reimbursed)
                .ok_or_else(|| invalid("outstanding cost overflow"))?;
            Ok(CostPosition {
                unit,
                incurred: incurred.to_string(),
                reimbursed: reimbursed.to_string(),
                outstanding: outstanding.to_string(),
            })
        })
        .collect()
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairMode {
    Preview,
    ApprovedSafe,
    ReviewedCorrection,
}
/// Approval binds to the exact proposed command digest and source revision.
/// An adapter may propose a match, but only this persisted approval permits it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairApproval {
    pub actor_id: Uuid,
    pub approved_at: String,
    pub command_digest: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationScope {
    pub firm_id: Uuid,
    pub period_start: String,
    pub period_end: String,
    pub sources: Vec<SourceReference>,
    pub complete_statement_document_id: Option<Uuid>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciliationRequest {
    #[serde(default)]
    pub signal_cursor: i64,
    pub id: Uuid,
    pub scope: ReconciliationScope,
    pub mode: RepairMode,
    pub snapshots: Vec<SourceSnapshot>,
    pub proposed_entries: Vec<JournalEntry>,
    pub approvals: Vec<RepairApproval>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationAudit {
    #[serde(default)]
    pub signal_cursor: i64,
    pub job_id: Uuid,
    pub scope: ReconciliationScope,
    pub approvals: Vec<RepairApproval>,
    /// Matching linked records never proves full bank/IOLTA statement coverage.
    pub complete_account_reconciliation: bool,
    pub firm_id: Uuid,
    pub at: String,
    pub mode: RepairMode,
    pub source_digests: Vec<String>,
    pub checks: Vec<Reconciliation>,
    pub posted_entries: Vec<Uuid>,
    pub exceptions: Vec<String>,
}
impl ReconciliationAudit {
    #[must_use]
    pub fn reconciled(&self) -> bool {
        self.exceptions.is_empty()
            && !self.checks.is_empty()
            && self.checks.iter().all(Reconciliation::reconciled)
    }
}
pub fn entry_digest(entry: &JournalEntry) -> Result<String, JournalError> {
    Ok(digest(serde_json::to_string(entry)?.as_bytes()))
}
/// Bounded repair command shared by manual runs and polling workers. Snapshots
/// and the audit are append-only. The same source/version/repair key cannot
/// repeat an economic event after a crash between posting and audit writing.
#[expect(
    clippy::too_many_lines,
    reason = "ordered evidence, approval, repair and revalidation phases form one audit"
)]
pub async fn reconcile(
    db: &SurrealDb,
    pg: &PostgresDb,
    request: &ReconciliationRequest,
) -> Result<ReconciliationAudit, JournalError> {
    let start = chrono::DateTime::parse_from_rfc3339(&request.scope.period_start)
        .map_err(|_| invalid("invalid reconciliation start"))?;
    let end = chrono::DateTime::parse_from_rfc3339(&request.scope.period_end)
        .map_err(|_| invalid("invalid reconciliation end"))?;
    if request.signal_cursor < 0
        || start >= end
        || request.scope.sources.is_empty()
        || request.scope.sources.len() > 1000
        || request.snapshots.len() > 1000
        || request.proposed_entries.len() > 1000
    {
        return Err(invalid("invalid or unbounded reconciliation scope"));
    }
    if let Some(id) = request.scope.complete_statement_document_id {
        evidence_firm(db, id, request.scope.firm_id).await?;
    }
    let firm = crate::firms::find_by_id(db, request.scope.firm_id)
        .await
        .map_err(|e| invalid(&e.to_string()))?
        .ok_or_else(|| invalid("Firm missing"))?;
    {
        let mut connection = pg.0.acquire().await?;
        ensure_firm_row(&mut connection, &firm).await?;
    }
    let mut audit = ReconciliationAudit {
        signal_cursor: request.signal_cursor,
        job_id: request.id,
        scope: request.scope.clone(),
        approvals: request.approvals.clone(),
        complete_account_reconciliation: false,
        firm_id: request.scope.firm_id,
        at: chrono::Utc::now().to_rfc3339(),
        mode: request.mode,
        source_digests: vec![],
        checks: vec![],
        posted_entries: vec![],
        exceptions: vec![],
    };
    let mut snapshots = BTreeMap::new();
    for s in &request.snapshots {
        if !request.scope.sources.contains(&s.source)
            || snapshots.insert(s.source.clone(), s).is_some()
        {
            return Err(invalid(
                "snapshot outside declared scope or duplicate source",
            ));
        }
        match s
            .occurred_at
            .as_deref()
            .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
        {
            Some(time) if time >= start && time < end => {}
            _ => audit
                .exceptions
                .push("source occurrence timestamp is missing or outside declared period".into()),
        }
        s.unit.validate()?;
        quantity(&s.quantity)?;
        let retrieved = chrono::DateTime::parse_from_rfc3339(&s.retrieved_at)
            .map_err(|_| invalid("invalid snapshot time"))?;
        if chrono::Utc::now()
            .signed_duration_since(retrieved)
            .num_seconds()
            > 300
            || retrieved > chrono::Utc::now() + chrono::Duration::seconds(30)
        {
            audit
                .exceptions
                .push("stale source snapshot requires refresh".into());
        }
        if s.digest.len() != 64
            || !s.digest.bytes().all(|b| b.is_ascii_hexdigit())
            || s.revision.trim().is_empty()
        {
            return Err(invalid("snapshot lacks revision/digest evidence"));
        }
        let mut digest_verified = false;
        for id in s
            .receipt_document_ids
            .iter()
            .chain(&s.evidence_document_ids)
        {
            if evidence_firm(db, *id, request.scope.firm_id)
                .await?
                .sha256_hex
                == s.digest
            {
                digest_verified = true;
            }
        }
        if !digest_verified {
            audit
                .exceptions
                .push("source snapshot lacks original digest-matching private evidence".into());
        }
        if let SourceFacts::Noncash {
            property_id,
            description,
            custody_reference,
            ..
        } = &s.facts
        {
            if !matches!(&s.unit, Unit::Noncash { property_id: unit_id, .. } if unit_id == property_id)
                || description.trim().is_empty()
                || custody_reference.trim().is_empty()
            {
                return Err(invalid(
                    "noncash property requires matching identity, description and custody evidence",
                ));
            }
        }
        if let SourceFacts::Crypto {
            fee_unit,
            fee_quantity,
            ..
        } = &s.facts
        {
            fee_unit.validate()?;
            if quantity(fee_quantity)? < 0 {
                return Err(invalid("crypto fee quantity cannot be negative"));
            }
            if quantity(fee_quantity)? > 0 {
                let source = SourceReference {
                    object_type: "network_fee".into(),
                    locator: None,
                    ..s.source.clone()
                };
                if !request.scope.sources.contains(&source)
                    || !request.snapshots.iter().any(|fee| {
                        fee.source == source
                            && matches!(fee.facts, SourceFacts::NetworkFee { .. })
                            && fee.unit == *fee_unit
                            && quantity(&fee.quantity).ok()
                                == quantity(fee_quantity).ok().and_then(i128::checked_neg)
                    })
                {
                    audit
                        .exceptions
                        .push("chain fee requires its separately scoped exact fee evidence".into());
                }
            }
        }
        if let SourceFacts::Crypto {
            finality_evidence_document_id,
            ..
        }
        | SourceFacts::NetworkFee {
            finality_evidence_document_id,
            ..
        } = &s.facts
        {
            evidence_firm(db, *finality_evidence_document_id, request.scope.firm_id).await?;
            if !s
                .receipt_document_ids
                .contains(finality_evidence_document_id)
                && !s
                    .evidence_document_ids
                    .contains(finality_evidence_document_id)
            {
                audit
                    .exceptions
                    .push("crypto finality evidence absent from snapshot receipts".into());
            }
        }
        if let Valuation::Known {
            reporting_unit,
            quantity,
            at,
            rate,
            rate_source,
            evidence_document_id,
        } = &s.valuation
        {
            reporting_unit.validate()?;
            crate::firm_journal::quantity(quantity)?;
            chrono::DateTime::parse_from_rfc3339(at)
                .map_err(|_| invalid("invalid valuation timestamp"))?;
            if crate::firm_journal::quantity(&parse_decimal(rate, 18)?)? <= 0 {
                return Err(invalid(
                    "valuation requires a positive exact reporting rate",
                ));
            }
            if rate_source.trim().is_empty() {
                return Err(invalid("valuation requires rate provenance"));
            }
            evidence_firm(db, *evidence_document_id, request.scope.firm_id).await?;
        }
        audit.source_digests.push(s.digest.clone());
        persist_snapshot(pg, request.scope.firm_id, s).await?;
    }
    for source in &request.scope.sources {
        if !snapshots.contains_key(source) {
            audit
                .exceptions
                .push("explicit source unavailable; no repair permitted".into());
        }
    }
    if request.scope.complete_statement_document_id.is_none() {
        audit
            .exceptions
            .push("complete account statement coverage is missing".into());
    }
    let safe = audit.exceptions.is_empty();
    for proposed in &request.proposed_entries {
        let occurred = chrono::DateTime::parse_from_rfc3339(&proposed.occurred_at)
            .map_err(|_| invalid("invalid repair occurrence timestamp"))?;
        if occurred < start || occurred >= end {
            return Err(invalid(
                "repair occurs outside the declared reconciliation period",
            ));
        }
        if proposed.firm_id != request.scope.firm_id
            || proposed.sources.is_empty()
            || proposed
                .sources
                .iter()
                .any(|s| !request.scope.sources.contains(s))
        {
            return Err(invalid("repair outside declared Firm/source scope"));
        }
        if proposed
            .sources
            .iter()
            .any(|source| !snapshots.contains_key(source))
        {
            audit
                .exceptions
                .push("proposed command source unavailable; no repair permitted".into());
            continue;
        }
        let hash = entry_digest(proposed)?;
        let approved = request.approvals.iter().any(|a| {
            a.command_digest == hash
                && a.actor_id == proposed.actor_id
                && chrono::DateTime::parse_from_rfc3339(&a.approved_at).is_ok()
        });
        let snapshot = proposed
            .sources
            .first()
            .and_then(|s| snapshots.get(s))
            .ok_or_else(|| invalid("repair lacks source"))?;
        let deterministic_key = format!(
            "repair:{}:{}:{}",
            digest(serde_json::to_string(&snapshot.source)?.as_bytes()),
            snapshot.revision,
            if proposed.reversal_of.is_some() {
                "reversal"
            } else {
                "posting"
            }
        );
        if proposed.key != deterministic_key {
            audit
                .exceptions
                .push("source revision or repair key changed; command requires review".into());
            continue;
        }
        let correction = proposed.reversal_of.is_some();
        let mut projected = proposed.clone();
        if let Some(original_id) = proposed.reversal_of {
            let history = entries(pg, request.scope.firm_id).await?;
            let original = history
                .iter()
                .find(|entry| entry.id == original_id)
                .ok_or_else(|| invalid("reviewed correction original is missing"))?;
            if !original.sources.contains(&snapshot.source) {
                return Err(invalid("correction source differs from original event"));
            }
            projected.postings.extend(original.postings.clone());
            projected.receipts.extend(original.receipts.clone());
            projected.reversal_of = None;
        }
        let source_status_allowed = if correction {
            snapshot.status == SourceStatus::Reversed
        } else {
            snapshot.status == SourceStatus::Completed
        };
        let allowed = safe
            && proposed.validate().is_ok()
            && proposed.sources.iter().all(|source| {
                snapshots
                    .get(source)
                    .is_some_and(|evidence| compare(&projected, evidence, true).reconciled())
            })
            && approved
            && source_status_allowed
            && request.mode != RepairMode::Preview
            && (!correction || request.mode == RepairMode::ReviewedCorrection);
        if allowed {
            audit.posted_entries.push(post(db, pg, proposed).await?);
        } else {
            audit
                .exceptions
                .push("proposed command requires fresh evidence and bound approval".into());
        }
    }
    let all = entries(pg, request.scope.firm_id).await?;
    for source in &request.scope.sources {
        let Some(snapshot) = snapshots.get(source) else {
            continue;
        };
        let originals: BTreeSet<_> = all
            .iter()
            .filter(|e| e.sources.contains(source))
            .map(|e| e.id)
            .collect();
        let matched: Vec<_> = all
            .iter()
            .filter(|e| {
                e.sources.contains(source)
                    || e.reversal_of.is_some_and(|id| originals.contains(&id))
            })
            .collect();
        if matched.is_empty() {
            audit
                .exceptions
                .push("source has no journal posting".into());
            continue;
        }
        // Aggregate shared sources before checking: installments and Project
        // allocations do not turn one provider object into duplicate proceeds.
        let mut combined = matched[0].clone();
        combined.postings = matched.iter().flat_map(|e| e.postings.clone()).collect();
        combined.receipts = matched.iter().flat_map(|e| e.receipts.clone()).collect();
        let check = compare(
            &combined,
            snapshot,
            request.scope.complete_statement_document_id.is_some(),
        );
        audit.checks.push(check);
    }
    persist_audit(pg, &audit).await?;
    Ok(audit)
}
async fn persist_snapshot(
    pg: &PostgresDb,
    firm: Uuid,
    s: &SourceSnapshot,
) -> Result<(), JournalError> {
    let mut tx = pg.0.begin().await?;
    let source = persist_source(&mut tx, firm, &s.source).await?;
    let id = digest(format!("{firm}:{}", serde_json::to_string(s)?).as_bytes());
    sqlx::query("INSERT INTO journal_source_snapshot(id,firm_id,source_id,revision,digest,retrieved_at,payload) VALUES($1,$2,$3,$4,$5,$6::timestamptz,$7) ON CONFLICT(id) DO NOTHING").bind(id).bind(firm).bind(source).bind(&s.revision).bind(&s.digest).bind(&s.retrieved_at).bind(sqlx::types::Json(s)).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
/// Bounded audit history is a durable checkpoint for operators and polling jobs.
pub async fn audits(
    pg: &PostgresDb,
    firm: Uuid,
    job: Option<Uuid>,
) -> Result<Vec<ReconciliationAudit>, JournalError> {
    let rows: Vec<(sqlx::types::Json<ReconciliationAudit>,)> = sqlx::query_as(
        "SELECT payload FROM journal_reconciliation WHERE firm_id=$1 AND ($2::uuid IS NULL OR job_id=$2) ORDER BY inserted_at DESC,id DESC LIMIT 100"
    ).bind(firm).bind(job).fetch_all(&pg.0).await?;
    Ok(rows.into_iter().map(|(audit,)| audit.0).collect())
}
async fn persist_audit(pg: &PostgresDb, a: &ReconciliationAudit) -> Result<(), JournalError> {
    sqlx::query(
        "INSERT INTO journal_reconciliation(id,job_id,firm_id,mode,payload) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(Uuid::new_v4())
    .bind(a.job_id)
    .bind(a.firm_id)
    .bind(
        serde_json::to_value(a.mode)?
            .as_str()
            .ok_or_else(|| invalid("mode encoding"))?,
    )
    .bind(sqlx::types::Json(a))
    .execute(&pg.0)
    .await?;
    Ok(())
}
async fn evidence_firm(
    db: &SurrealDb,
    document: Uuid,
    firm: Uuid,
) -> Result<crate::assets::Asset, JournalError> {
    let asset = crate::assets::find_by_id(db, document)
        .await
        .map_err(|e| invalid(&e.to_string()))?
        .ok_or_else(|| invalid("source evidence Document missing"))?;
    let project = asset
        .project_id
        .ok_or_else(|| invalid("source evidence is not Project scoped"))?;
    if crate::projects::find_by_id(db, project)
        .await
        .map_err(|e| invalid(&e.to_string()))?
        .and_then(|p| p.firm_id)
        != Some(firm)
        || asset.deleted_at.is_some()
    {
        return Err(invalid("source evidence is unavailable in this Firm"));
    }
    Ok(asset)
}

/// A verified wake-up signal, never authority for a financial write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationSignal {
    pub cursor: i64,
    pub source: SourceReference,
}
/// Persist only events for already-linked objects in this Firm. Event URLs are never followed.
#[expect(
    clippy::too_many_arguments,
    reason = "verified provider event fields are independently scoped and bound"
)]
pub async fn signal(
    pg: &PostgresDb,
    firm: Uuid,
    provider: &str,
    scope: &str,
    object_type: &str,
    external_id: &str,
    event_id: &str,
    occurred_at: &str,
) -> Result<u64, JournalError> {
    if event_id.len() != 64 || !event_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("signal lacks its content identity"));
    }
    chrono::DateTime::parse_from_rfc3339(occurred_at)
        .map_err(|_| invalid("invalid signal timestamp"))?;
    let result = sqlx::query("INSERT INTO journal_reconciliation_signal(firm_id,source_id,event_id,occurred_at) SELECT firm_id,id,$6,$7::timestamptz FROM journal_source_reference WHERE firm_id=$1 AND provider=$2 AND provider_scope=$3 AND object_type=$4 AND external_id=$5 ON CONFLICT(firm_id,source_id,event_id) DO NOTHING")
        .bind(firm).bind(provider).bind(scope).bind(object_type).bind(external_id).bind(event_id).bind(occurred_at).execute(&pg.0).await?;
    Ok(result.rows_affected())
}
/// Bounded, Firm-scoped cursor read used by polling workers and operator monitors.
pub async fn signals(
    pg: &PostgresDb,
    firm: Uuid,
    after: i64,
) -> Result<Vec<ReconciliationSignal>, JournalError> {
    use sqlx::Row;
    if after < 0 {
        return Err(invalid("negative signal cursor"));
    }
    let rows = sqlx::query("SELECT signal.id,source.payload FROM journal_reconciliation_signal signal JOIN journal_source_reference source ON source.id=signal.source_id AND source.firm_id=signal.firm_id WHERE signal.firm_id=$1 AND signal.id>$2 ORDER BY signal.id LIMIT 200")
        .bind(firm).bind(after).fetch_all(&pg.0).await?;
    rows.into_iter()
        .map(|row| {
            Ok(ReconciliationSignal {
                cursor: row.try_get("id")?,
                source: row
                    .try_get::<sqlx::types::Json<SourceReference>, _>("payload")?
                    .0,
            })
        })
        .collect()
}
