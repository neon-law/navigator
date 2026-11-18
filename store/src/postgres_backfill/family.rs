//! Versioned table enrollment on the existing Mirror Backfill ledger.

use crate::postgres::PostgresDb;
use sqlx::Row;
use uuid::Uuid;

/// A bounded set of tables that must acquire history before PostgreSQL authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Independent matter, review, credential, and publication satellites.
    Satellites,
    /// Remaining application catalogs, intake, service, and approval records.
    Application,
    /// Person identity and mailbox claims.
    Identity,
    /// Firm, Brand, Project, and participation authority.
    Tenancy,
    /// Notation writers and their canonical document and Asset protection facts.
    Notation,
}

impl Family {
    /// Every registered family required before application readiness or workflow effects.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Satellites,
            Self::Notation,
            Self::Identity,
            Self::Tenancy,
            Self::Application,
        ]
    }

    /// The immutable version of this family's import contract.
    #[must_use]
    pub const fn scope(self) -> &'static str {
        match self {
            Self::Satellites => "satellites:1",
            Self::Notation => "notation:1",
            Self::Application => "application:1",
            Self::Identity => "identity:1",
            Self::Tenancy => "tenancy:1",
        }
    }

    /// The exact enrollment set, in parent-before-child order.
    #[must_use]
    pub const fn tables(self) -> &'static [&'static str] {
        match self {
            Self::Satellites => &[
                "attestation",
                "authority_use",
                "citation",
                "contract_review",
                "efiling_transactions",
                "expunge_record",
                "expunge_request",
                "git_access_token",
                "matter_caption",
                "playbook",
                "project_module",
                "social_oauth_attempt",
                "testimonial",
            ],
            Self::Identity => &[
                "entity",
                "person",
                "person_mailbox",
                "address",
                "credential",
                "email_token",
                "person_delegate",
                "person_external_identity",
                "person_phone",
            ],
            Self::Tenancy => &[
                "firm",
                "brand",
                "firm_brand",
                "person_firm_role",
                "project",
                "person_project_role",
            ],
            Self::Application => &[
                "template",
                "mailroom",
                "letter",
                "lead",
                "lead_consent",
                "project_deadlines",
                "project_service",
                "summary_sender_approval",
                "summary_sender_approval_event",
                "inbox_triage",
                "deployment_secret_change",
                "matter_page",
                "notation_document_comment",
                "firm_integration_secret",
            ],
            Self::Notation => &[
                "notation",
                "answer",
                "notation_event",
                "notation_clause",
                "notation_source",
                "asset",
                "notation_document",
                "notation_document_version",
            ],
        }
    }

    /// Resolve the immutable version recorded on a run.
    #[must_use]
    pub fn from_scope(scope: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|family| family.scope() == scope)
    }

    /// The current import contract for a table, if it needs scoped enrollment.
    #[must_use]
    pub fn for_table(table: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|family| family.tables().contains(&table))
    }
}

/// A source write's permitted behavior; active authority is never replayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// Refresh a pending enrollment under the existing backfill lock.
    Refresh,
    /// The first global import may insert only identities PostgreSQL lacks.
    InsertMissing,
    /// No source read, update, or deletion is permitted.
    Refuse,
}

/// A scoped import is unavailable or its content-free enrollment evidence is incomplete.
#[derive(Debug, thiserror::Error)]
pub enum FamilyError {
    /// PostgreSQL could not read or write the ledger.
    #[error("PostgreSQL family enrollment is unavailable")]
    Query(#[from] sqlx::Error),
    /// Every registered table must be enrolled together before authority is selected.
    #[error("PostgreSQL family history has not been activated")]
    NotActive,
    /// An earlier import already owns a table; enrollment cannot replay its history.
    #[error("a family table is already enrolled")]
    Overlap,
    /// An activated capture is immutable.
    #[error("an activated family capture is immutable")]
    Activated,
    /// The supplied run does not match the exact registered enrollment set.
    #[error("the family enrollment receipt is incomplete")]
    Receipt,
}

tokio::task_local! {
    static IMPORTING: (Uuid, Family);
}

/// Whether this task is performing the table's pending scoped import.
#[must_use]
pub(crate) fn importing(table: &str) -> bool {
    IMPORTING
        .try_with(|(_, family)| family.tables().contains(&table))
        .unwrap_or(false)
}

pub(super) async fn during_import<T>(
    run: Uuid,
    family: Family,
    future: impl std::future::Future<Output = T>,
) -> T {
    IMPORTING.scope((run, family), future).await
}

fn checkpoint(table: &str) -> String {
    format!("table:{table}")
}

/// Whether any completed scoped receipt owns this exact table.
///
/// Ownership is looked up by persisted table enrollment, rather than only by the
/// current family version, so a later registry cannot resurrect deleted rows.
///
/// # Errors
/// PostgreSQL could not read the existing backfill ledger.
pub async fn active_for_table(pg: &PostgresDb, table: &str) -> Result<bool, FamilyError> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM mirror_backfill_run r \
         JOIN mirror_backfill_checkpoint c ON c.run_id = r.id \
         WHERE r.scope <> 'global' AND r.status = 'completed' \
         AND c.mirror = $1 AND c.completed_at IS NOT NULL)",
    )
    .bind(checkpoint(table))
    .fetch_one(&pg.0)
    .await?)
}

/// Require one completed receipt whose exact table set matches this family.
///
/// # Errors
/// The ledger is unreadable or historical import and activation are incomplete.
pub async fn require_scope(pg: &PostgresDb, family: Family) -> Result<(), FamilyError> {
    validated_scope(pg, family).await.map(|_| ())
}

/// Require all registered historical imports before serving application traffic or effects.
/// Database connections and operator import/report commands deliberately do not call this.
///
/// # Errors
/// Any required family is missing, malformed, ambiguous, or PostgreSQL is unavailable.
pub async fn require_runtime_scopes(pg: &PostgresDb) -> Result<(), FamilyError> {
    let completed = validated_completed_scopes(pg).await?;
    if Family::all()
        .iter()
        .any(|family| !completed.contains(family))
    {
        return Err(FamilyError::NotActive);
    }
    Ok(())
}

/// Protect global mutations without requiring a not-yet-enrolled family.
/// Read-only reports, recovery inspection, and connection construction remain independent.
///
/// # Errors
/// A completed scope is unsupported or its existing activation evidence is invalid.
pub async fn require_supported_scopes(pg: &PostgresDb) -> Result<(), FamilyError> {
    validated_completed_scopes(pg).await.map(|_| ())
}

async fn validated_completed_scopes(pg: &PostgresDb) -> Result<Vec<Family>, FamilyError> {
    let scopes: Vec<String> = sqlx::query_scalar("SELECT DISTINCT scope FROM mirror_backfill_run WHERE scope <> 'global' AND status = 'completed'")
        .fetch_all(&pg.0).await?;
    let mut completed = Vec::new();
    for scope in scopes {
        let family = Family::from_scope(&scope).ok_or(FamilyError::Receipt)?;
        require_scope(pg, family).await?;
        completed.push(family);
    }
    Ok(completed)
}

async fn validated_scope(pg: &PostgresDb, family: Family) -> Result<Uuid, FamilyError> {
    let runs = sqlx::query("SELECT id, source_receipt FROM mirror_backfill_run WHERE scope = $1 AND status = 'completed'")
        .bind(family.scope()).fetch_all(&pg.0).await?;
    if runs.is_empty() {
        return Err(FamilyError::NotActive);
    }
    let [run] = runs.as_slice() else {
        return Err(FamilyError::Receipt);
    };
    let run_id: Uuid = run.try_get("id")?;
    let names: Vec<_> = family
        .tables()
        .iter()
        .map(|table| checkpoint(table))
        .collect();
    let overlapping: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mirror_backfill_run r JOIN mirror_backfill_checkpoint c ON c.run_id = r.id WHERE r.status = 'completed' AND r.id <> $1 AND c.mirror = ANY($2::text[]))")
        .bind(run_id).bind(names).fetch_one(&pg.0).await?;
    if overlapping {
        return Err(FamilyError::Receipt);
    }
    let receipt: SourceReceipt =
        serde_json::from_value(run.try_get("source_receipt")?).map_err(|_| FamilyError::Receipt)?;
    receipt.validate(family)?;
    let rows = sqlx::query("SELECT mirror, completed_at IS NOT NULL AS completed, verification FROM mirror_backfill_checkpoint WHERE run_id = $1")
        .bind(run.try_get::<Uuid, _>("id")?).fetch_all(&pg.0).await?;
    if rows.len() != family.tables().len() {
        return Err(FamilyError::Receipt);
    }
    let mut seen = std::collections::BTreeSet::new();
    for row in rows {
        let name: String = row.try_get("mirror")?;
        let table = name
            .strip_prefix("table:")
            .filter(|table| family.tables().contains(table))
            .ok_or(FamilyError::Receipt)?;
        if !seen.insert(table.to_owned()) || !row.try_get::<bool, _>("completed")? {
            return Err(FamilyError::Receipt);
        }
        let proof: Verification = serde_json::from_value(row.try_get("verification")?)
            .map_err(|_| FamilyError::Receipt)?;
        let captured = usize::try_from(
            *receipt
                .table_counts
                .get(table)
                .ok_or(FamilyError::Receipt)?,
        )
        .map_err(|_| FamilyError::Receipt)?;
        if family == Family::Notation
            && proof
                .reset_union
                .as_ref()
                .and_then(|proof| i64::try_from(proof.compared).ok())
                != receipt.table_counts.get("notation").copied()
        {
            return Err(FamilyError::Receipt);
        }
        if family == Family::Notation
            && !proof.template_pins.as_ref().is_some_and(|pins| {
                receipt
                    .table_counts
                    .get("notation")
                    .and_then(|count| usize::try_from(*count).ok())
                    .is_some_and(|count| pins.clean(count))
            })
        {
            return Err(FamilyError::Receipt);
        }
        if !proof.clean(family, captured, table) {
            return Err(FamilyError::Receipt);
        }
    }
    Ok(run.try_get("id")?)
}

/// Restore the immutable completion proof without consulting the legacy source.
pub(super) async fn completed_proof(
    pg: &PostgresDb,
    family: Family,
    run: Uuid,
) -> Result<
    (
        Vec<super::ParityTableReport>,
        Vec<crate::postgres_mirror::records::UnresolvedReference>,
    ),
    FamilyError,
> {
    if validated_scope(pg, family).await? != run {
        return Err(FamilyError::Receipt);
    }
    let mut tables = Vec::new();
    let mut references = Vec::new();
    for table in family.tables() {
        let value: serde_json::Value = sqlx::query_scalar(
            "SELECT verification FROM mirror_backfill_checkpoint WHERE run_id = $1 AND mirror = $2",
        )
        .bind(run)
        .bind(checkpoint(table))
        .fetch_one(&pg.0)
        .await?;
        let proof: Verification =
            serde_json::from_value(value).map_err(|_| FamilyError::Receipt)?;
        for reference in &proof.references {
            references.push(crate::postgres_mirror::records::UnresolvedReference {
                table,
                field: crate::postgres_mirror::records::reference_field(table, &reference.field)
                    .ok_or(FamilyError::Receipt)?,
                source_orphans: reference.source_orphans,
                postgres_missing: reference.postgres_missing,
                newly_target_missing: reference.newly_target_missing,
            });
        }
        tables.push(super::ParityTableReport {
            mirror: family.scope().into(),
            table: (*table).into(),
            surreal: proof.source_rows,
            postgres: proof.target_rows,
            missing: 0,
            differing: 0,
            extra: 0,
            divergent_ids: vec![],
            omitted_divergent_ids: 0,
            withheld_non_uuid_keys: 0,
            asset_union: proof.asset_union,
            reset_union: (*table == "notation")
                .then_some(proof.reset_union)
                .flatten(),
            template_pins: (*table == "notation")
                .then_some(proof.template_pins)
                .flatten(),
        });
    }
    Ok((tables, references))
}

/// Determine permission before fetching any source row for this table.
///
/// # Errors
/// PostgreSQL could not read the ledger or the pending import receipt is invalid.
pub async fn import_mode(pg: &PostgresDb, table: &str) -> Result<ImportMode, FamilyError> {
    if active_for_table(pg, table).await? {
        return Ok(ImportMode::Refuse);
    }
    if Family::for_table(table).is_none() {
        return Ok(ImportMode::Refuse);
    }
    let scope = IMPORTING.try_with(|scope| *scope).ok();
    if let Some((run, family)) = scope.filter(|(_, family)| family.tables().contains(&table)) {
        let pending: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM mirror_backfill_run r \
             JOIN mirror_backfill_checkpoint c ON c.run_id = r.id \
             WHERE r.id = $1 AND r.scope = $2 AND r.status = 'running' AND c.mirror = $3)",
        )
        .bind(run)
        .bind(family.scope())
        .bind(checkpoint(table))
        .fetch_one(&pg.0)
        .await?;
        return if pending {
            Ok(ImportMode::Refresh)
        } else {
            Err(FamilyError::Receipt)
        };
    }
    Ok(if crate::postgres_mirror::initial_copy() {
        ImportMode::InsertMissing
    } else {
        ImportMode::Refuse
    })
}

/// Persist exact table enrollment before any source rows are copied.
/// Caller holds the existing session advisory lock throughout this transaction
/// and every following import and activation operation.
pub(super) async fn enroll(
    pg: &PostgresDb,
    run: Uuid,
    family: Family,
    batch_size: i32,
    source_receipt: VerifiedSourceReceipt,
) -> Result<(), FamilyError> {
    source_receipt.0.validate(family)?;
    let source_receipt =
        serde_json::to_value(source_receipt.0).map_err(|_| FamilyError::Receipt)?;
    let names: Vec<String> = family
        .tables()
        .iter()
        .map(|table| checkpoint(table))
        .collect();
    let mut tx = pg.0.begin().await?;
    let overlaps: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM mirror_backfill_checkpoint c \
         JOIN mirror_backfill_run r ON r.id = c.run_id \
         WHERE r.scope <> 'global' AND c.mirror = ANY($1::text[]))",
    )
    .bind(&names)
    .fetch_one(&mut *tx)
    .await?;
    if overlaps {
        return Err(FamilyError::Overlap);
    }
    sqlx::query(
        "INSERT INTO mirror_backfill_run (id, batch_size, status, scope, source_receipt) \
         VALUES ($1, $2, 'running', $3, $4)",
    )
    .bind(run)
    .bind(batch_size)
    .bind(family.scope())
    .bind(source_receipt)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO mirror_backfill_checkpoint (run_id, mirror) \
         SELECT $1, unnest($2::text[])",
    )
    .bind(run)
    .bind(names)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Read the persisted enrollment without interpreting a global run as a family.
pub(super) async fn enrolled_family(pg: &PostgresDb, run: Uuid) -> Result<Family, FamilyError> {
    let row = sqlx::query("SELECT scope FROM mirror_backfill_run WHERE id = $1")
        .bind(run)
        .fetch_optional(&pg.0)
        .await?
        .ok_or(FamilyError::Receipt)?;
    let scope: String = row.try_get("scope")?;
    Family::from_scope(&scope).ok_or(FamilyError::Receipt)
}

/// Content-free source capture evidence from the existing operational archive receipt.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SourceReceipt {
    /// Existing operational archive receipt format.
    pub format_version: u32,
    /// Must be the existing strict export's `operator_quiesced` value.
    pub consistency: String,
    /// Number of bytes in the captured export.
    pub export_bytes: u64,
    /// SHA-256 of the captured export, with no source contents.
    pub export_sha256: String,
    /// SHA-256 of the source schema at capture.
    pub schema_sha256: String,
    /// Raw source census captured while every old writer remains frozen.
    pub table_counts: std::collections::BTreeMap<String, i64>,
}

impl SourceReceipt {
    /// Reject best-effort, malformed, or incomplete archive evidence.
    /// This validates capture evidence, not an ongoing writer freeze: the operator
    /// keeps all old application and worker writers quiesced through activation.
    ///
    /// # Errors
    /// The existing strict archive receipt does not cover this exact table set.
    pub fn validate(&self, family: Family) -> Result<(), FamilyError> {
        let digest =
            |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
        if self.format_version != 1
            || self.consistency != "operator_quiesced"
            || self.export_bytes == 0
            || !digest(&self.export_sha256)
            || !digest(&self.schema_sha256)
            || self.table_counts.values().any(|count| *count < 0)
            || family
                .tables()
                .iter()
                .any(|table| !self.table_counts.contains_key(*table))
        {
            return Err(FamilyError::Receipt);
        }
        Ok(())
    }
}

/// Full Asset union accounting for the scoped handoff. PostgreSQL-only rows
/// are accepted only with independent native onboarding evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetUnionProof {
    pub source_only_remaining: usize,
    pub compared_overlaps: usize,
    pub resolved_native_overlaps: usize,
    pub unresolved_overlaps: usize,
    pub proven_native_target_only: usize,
    pub unknown_target_only: usize,
    pub full_metadata_compared: bool,
}

impl AssetUnionProof {
    fn valid(&self, source: usize, target: usize, differences: usize) -> bool {
        self.full_metadata_compared
            && self
                .source_only_remaining
                .checked_add(self.compared_overlaps)
                == Some(source)
            && self
                .compared_overlaps
                .checked_add(self.proven_native_target_only)
                .and_then(|count| count.checked_add(self.unknown_target_only))
                == Some(target)
            && self
                .resolved_native_overlaps
                .checked_add(self.unresolved_overlaps)
                .is_some_and(|count| count <= self.compared_overlaps)
            && self
                .source_only_remaining
                .checked_add(self.unresolved_overlaps)
                .and_then(|count| count.checked_add(self.unknown_target_only))
                == Some(differences)
    }
}

/// Extra PostgreSQL reset protection proven by independently native Assets.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResetUnionProof {
    pub compared: usize,
    pub retained_native_protections: usize,
}

/// Referenced immutable Template pins checked against the same frozen capture.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplatePinProof {
    pub compared: usize,
    pub differences: usize,
}

impl TemplatePinProof {
    fn clean(&self, notations: usize) -> bool {
        self.compared <= notations
            && (self.compared == 0) == (notations == 0)
            && self.differences == 0
    }
}

/// Final snapshot comparisons, with only table names, counts, and row identities.
pub(super) struct Snapshot {
    pub schema_sha256: String,
    pub export_sha256: String,
    pub export_bytes: u64,
    pub tables: Vec<crate::postgres_mirror::TableParity>,
    pub raw_counts: std::collections::BTreeMap<String, usize>,
    pub references: Vec<crate::postgres_mirror::records::UnresolvedReference>,
    pub reset_fact_differences: Vec<Uuid>,
    pub foreign_keys_hold: bool,
    pub asset_union: Option<AssetUnionProof>,
    pub reset_union: Option<ResetUnionProof>,
    pub template_pins: Option<TemplatePinProof>,
}

impl Snapshot {
    fn representable(&self, family: Family, receipt: &SourceReceipt) -> bool {
        let actual: std::collections::BTreeSet<_> =
            self.tables.iter().map(|table| table.table).collect();
        let expected: std::collections::BTreeSet<_> = family.tables().iter().copied().collect();
        self.schema_sha256 == receipt.schema_sha256
            && self.export_sha256 == receipt.export_sha256
            && self.export_bytes == receipt.export_bytes
            && self.tables.len() == expected.len()
            && actual == expected
            && match family {
                Family::Satellites | Family::Application | Family::Identity | Family::Tenancy => {
                    self.asset_union.is_none()
                        && self.reset_union.is_none()
                        && self.template_pins.is_none()
                }
                Family::Notation => {
                    self.tables
                        .iter()
                        .find(|table| table.table == "asset")
                        .zip(self.asset_union.as_ref())
                        .is_some_and(|(table, proof)| {
                            proof.valid(table.source_rows, table.target_rows, table.divergent.len())
                        })
                        && self.reset_union.as_ref().is_some_and(|proof| {
                            self.raw_counts.get("notation") == Some(&proof.compared)
                                && proof.retained_native_protections <= proof.compared
                        })
                }
            }
            && (family != Family::Notation || self.template_pins.is_some())
            && self.tables.iter().all(|table| {
                table.divergent.iter().all(|key| key.table() == table.table)
                    && self.raw_counts.get(table.table) == Some(&table.source_rows)
                    && receipt
                        .table_counts
                        .get(table.table)
                        .and_then(|count| usize::try_from(*count).ok())
                        == Some(table.source_rows)
            })
    }

    fn clean(&self) -> bool {
        self.foreign_keys_hold
            && self.tables.iter().all(|table| {
                table.divergent.is_empty()
                    && if table.table == "asset" {
                        self.asset_union.as_ref().is_some_and(|proof| {
                            proof.valid(table.source_rows, table.target_rows, 0)
                        })
                    } else {
                        table.source_rows == table.target_rows
                    }
            })
            && self
                .references
                .iter()
                .all(|reference| reference.newly_target_missing == 0)
            && self.reset_fact_differences.is_empty()
            && self.template_pins.as_ref().is_none_or(|proof| {
                proof.clean(self.raw_counts.get("notation").copied().unwrap_or_default())
            })
    }
}

type SourceOp<'a, T> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<T, super::BackfillError>> + Send + 'a>,
>;

/// Reuses production extraction and row-write seams; synthetic tests supply source
/// snapshots here so enrollment is proven without starting a legacy engine.
pub(super) trait Source: Sync {
    fn snapshot<'a>(&'a self, pg: &'a PostgresDb, family: Family) -> SourceOp<'a, Snapshot>;
    fn write<'a>(
        &'a self,
        pg: &'a PostgresDb,
        key: &'a crate::postgres_mirror::MirrorKey,
    ) -> SourceOp<'a, ()>;
}

/// Replace only a pending capture after checking the newly frozen source.
/// Identical verified captures preserve progress and do not duplicate history.
pub(super) async fn refresh_capture_with(
    source: &impl Source,
    pg: &PostgresDb,
    run: Uuid,
    receipt: VerifiedSourceReceipt,
) -> Result<(), super::BackfillError> {
    let family = enrolled_family(pg, run).await?;
    receipt.0.validate(family)?;
    let row = sqlx::query("SELECT status, source_receipt FROM mirror_backfill_run WHERE id = $1")
        .bind(run)
        .fetch_one(&pg.0)
        .await?;
    if row.try_get::<String, _>("status")? != "running" {
        return Err(FamilyError::Receipt.into());
    }
    let snapshot = source.snapshot(pg, family).await?;
    if !snapshot.representable(family, &receipt.0) {
        return Err(FamilyError::Receipt.into());
    }
    let replacement = serde_json::to_value(receipt.0).map_err(|_| FamilyError::Receipt)?;
    if row.try_get::<serde_json::Value, _>("source_receipt")? == replacement {
        return Ok(());
    }
    let mut tx = pg.0.begin().await?;
    let changed = sqlx::query("UPDATE mirror_backfill_run SET source_receipt_history = source_receipt_history || jsonb_build_array(source_receipt), source_receipt = $2, updated_at = now() WHERE id = $1 AND status = 'running'")
        .bind(run).bind(replacement).execute(&mut *tx).await?;
    if changed.rows_affected() != 1 {
        return Err(FamilyError::Receipt.into());
    }
    sqlx::query("UPDATE mirror_backfill_checkpoint SET verification = NULL, completed_at = NULL, updated_at = now() WHERE run_id = $1")
        .bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

pub(super) struct Advancement {
    pub batches: usize,
    pub snapshot: Option<Snapshot>,
}

/// Advance only this persisted enrollment; a completed receipt is read-only.
/// The caller holds the existing advisory lock and establishes `during_import`.
pub(super) async fn advance_with(
    source: &impl Source,
    pg: &PostgresDb,
    run: Uuid,
    max_batches: Option<usize>,
    activate: bool,
) -> Result<Advancement, super::BackfillError> {
    let family = enrolled_family(pg, run).await?;
    let row = sqlx::query(
        "SELECT status, batch_size, source_receipt FROM mirror_backfill_run WHERE id = $1",
    )
    .bind(run)
    .fetch_one(&pg.0)
    .await?;
    if row.try_get::<String, _>("status")? == "completed" {
        if validated_scope(pg, family).await? != run {
            return Err(FamilyError::Receipt.into());
        }
        return Ok(Advancement {
            batches: 0,
            snapshot: None,
        });
    }
    let receipt: SourceReceipt =
        serde_json::from_value(row.try_get("source_receipt")?).map_err(|_| FamilyError::Receipt)?;
    receipt.validate(family)?;
    let batch_size = usize::try_from(row.try_get::<i32, _>("batch_size")?)
        .ok()
        .filter(|size| *size > 0)
        .ok_or(super::BackfillError::BatchSize)?;
    let initial = source.snapshot(pg, family).await?;
    if !initial.representable(family, &receipt) {
        return Err(FamilyError::Receipt.into());
    }
    // A retry recomputes divergence. Successful rows from an interrupted batch
    // are already equal; stale rows are refreshed only while this scope is pending.
    let mut batches = 0;
    'tables: for table_name in family.tables() {
        let table = initial
            .tables
            .iter()
            .find(|table| table.table == *table_name)
            .ok_or(FamilyError::Receipt)?;
        for batch in table.divergent.chunks(batch_size) {
            if max_batches.is_some_and(|limit| batches >= limit) {
                break 'tables;
            }
            for key in batch {
                // Never infer a source deletion from a divergent target-only identity.
                // Production source.write fails closed if the source record is absent.
                apply_row(source, pg, run, key).await?;
            }
            let names: Vec<_> = batch.iter().map(|key| checkpoint(key.table())).collect();
            sqlx::query("UPDATE mirror_backfill_checkpoint SET batches = batches + 1, updated_at = now() WHERE run_id = $1 AND mirror = ANY($2::text[])")
            .bind(run).bind(names).execute(&pg.0).await?;
            batches += 1;
        }
    }
    drop(initial);
    let final_snapshot = source.snapshot(pg, family).await?;
    if !final_snapshot.representable(family, &receipt) {
        return Err(FamilyError::Receipt.into());
    }
    if activate && final_snapshot.clean() {
        // Complete all table checkpoints and their owning receipt atomically.
        // The advisory lock excludes other importers; operator-maintained writer
        // quiescence, not that lock, excludes old source application writes.
        let mut tx = pg.0.begin().await?;
        for table in &final_snapshot.tables {
            let proof = serde_json::to_value(Verification {
                source_rows: table.source_rows,
                target_rows: table.target_rows,
                differences: 0,
                references_checked: true,
                foreign_keys_checked: true,
                references: final_snapshot
                    .references
                    .iter()
                    .filter(|reference| reference.table == table.table)
                    .map(|reference| ReferenceVerification {
                        field: reference.field.to_owned(),
                        source_orphans: reference.source_orphans,
                        postgres_missing: reference.postgres_missing,
                        newly_target_missing: reference.newly_target_missing,
                    })
                    .collect(),
                reset_facts_checked: family == Family::Notation,
                reset_fact_differences: final_snapshot.reset_fact_differences.len(),
                asset_union: (table.table == "asset")
                    .then(|| final_snapshot.asset_union.clone())
                    .flatten(),
                reset_union: final_snapshot.reset_union.clone(),
                template_pins: final_snapshot.template_pins.clone(),
            })
            .map_err(|_| FamilyError::Receipt)?;
            sqlx::query("UPDATE mirror_backfill_checkpoint SET completed_at = now(), verification = $3, failed = 0, updated_at = now() WHERE run_id = $1 AND mirror = $2")
                .bind(run).bind(checkpoint(table.table)).bind(proof).execute(&mut *tx).await?;
        }
        sqlx::query("DELETE FROM mirror_backfill_failure WHERE run_id = $1")
            .bind(run)
            .execute(&mut *tx)
            .await?;
        sqlx::query("UPDATE mirror_backfill_run SET status = 'completed', completed_at = now(), updated_at = now() WHERE id = $1 AND status = 'running'")
            .bind(run).execute(&mut *tx).await?;
        tx.commit().await?;
    }
    Ok(Advancement {
        batches,
        snapshot: Some(final_snapshot),
    })
}

async fn apply_row(
    source: &impl Source,
    pg: &PostgresDb,
    run: Uuid,
    key: &crate::postgres_mirror::MirrorKey,
) -> Result<(), super::BackfillError> {
    let written = source.write(pg, key).await.is_ok();
    sqlx::query(
        "DELETE FROM mirror_backfill_failure WHERE run_id = $1 AND row_table = $2 AND row_id = $3",
    )
    .bind(run)
    .bind(key.table())
    .bind(key.report_id())
    .execute(&pg.0)
    .await?;
    if written {
        sqlx::query("UPDATE mirror_backfill_checkpoint SET applied = applied + 1, updated_at = now() WHERE run_id = $1 AND mirror = $2")
            .bind(run).bind(checkpoint(key.table())).execute(&pg.0).await?;
    } else {
        sqlx::query("INSERT INTO mirror_backfill_failure (id, run_id, mirror, row_table, row_id, reason) VALUES ($1,$2,$3,$4,$5,'family row import failed')")
            .bind(Uuid::new_v4()).bind(run).bind(checkpoint(key.table())).bind(key.table()).bind(key.report_id()).execute(&pg.0).await?;
    }
    sqlx::query("UPDATE mirror_backfill_checkpoint SET failed = (SELECT count(*)::integer FROM mirror_backfill_failure WHERE run_id = $1 AND mirror = $2), updated_at = now() WHERE run_id = $1 AND mirror = $2")
        .bind(run).bind(checkpoint(key.table())).execute(&pg.0).await?;
    Ok(())
}

/// An operational archive receipt bound to the exact captured export bytes.
/// Constructed only by [`SourceReceipt::verify_export`].
#[derive(Debug)]
pub struct VerifiedSourceReceipt(SourceReceipt);

impl SourceReceipt {
    /// Bind the existing receipt to its captured export before accepting enrollment.
    /// Best-effort captures remain usable for recovery; family enrollment additionally
    /// requires [`Self::validate`] and the operator's uninterrupted writer freeze.
    ///
    /// # Errors
    /// The export bytes do not match the canonical receipt's format, size, or digest.
    pub fn verify_export(self, export: &[u8]) -> Result<VerifiedSourceReceipt, FamilyError> {
        self.verify_export_reader(std::io::Cursor::new(export))
    }

    /// Bind a captured export without buffering its contents in memory.
    ///
    /// # Errors
    /// Reading fails or the export does not match the existing canonical receipt.
    pub fn verify_export_reader(
        self,
        mut export: impl std::io::Read,
    ) -> Result<VerifiedSourceReceipt, FamilyError> {
        use sha2::Digest;
        let mut hash = sha2::Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = export.read(&mut buffer).map_err(|_| FamilyError::Receipt)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
            bytes = bytes
                .checked_add(u64::try_from(read).map_err(|_| FamilyError::Receipt)?)
                .ok_or(FamilyError::Receipt)?;
        }
        if self.format_version != 1
            || !matches!(
                self.consistency.as_str(),
                "operator_quiesced" | "best_effort"
            )
            || self.export_bytes != bytes
            || self.export_sha256 != hex::encode(hash.finalize())
        {
            return Err(FamilyError::Receipt);
        }
        Ok(VerifiedSourceReceipt(self))
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceVerification {
    field: String,
    source_orphans: usize,
    postgres_missing: usize,
    newly_target_missing: usize,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Verification {
    source_rows: usize,
    target_rows: usize,
    differences: usize,
    references_checked: bool,
    foreign_keys_checked: bool,
    references: Vec<ReferenceVerification>,
    reset_facts_checked: bool,
    reset_fact_differences: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    asset_union: Option<AssetUnionProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reset_union: Option<ResetUnionProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    template_pins: Option<TemplatePinProof>,
}

impl Verification {
    fn clean(&self, family: Family, captured: usize, table: &str) -> bool {
        self.source_rows == captured
            && if family == Family::Notation && table == "asset" {
                self.asset_union
                    .as_ref()
                    .is_some_and(|proof| proof.valid(captured, self.target_rows, self.differences))
            } else {
                self.target_rows == captured && self.asset_union.is_none()
            }
            && match family {
                Family::Satellites | Family::Application | Family::Identity | Family::Tenancy => {
                    self.reset_union.is_none() && self.template_pins.is_none()
                }
                Family::Notation => {
                    self.reset_facts_checked
                        && self.reset_union.as_ref().is_some_and(|proof| {
                            proof.retained_native_protections <= proof.compared
                        })
                }
            }
            && self.differences == 0
            && self.references_checked
            && self.foreign_keys_checked
            && self.reset_fact_differences == 0
            && self.references.iter().all(|reference| {
                crate::postgres_mirror::records::reference_field(table, &reference.field).is_some()
                    && reference.source_orphans <= captured
                    && reference.postgres_missing <= captured
                    && reference.newly_target_missing == 0
            })
    }
}

/// Seed explicit completed enrollment evidence for synthetic native-accessor tests.
/// Import acceptance exercises the real driver separately; this is never a runtime bypass.
#[cfg(any(test, feature = "test-support"))]
pub(crate) async fn activate_fixture(pg: &PostgresDb, family: Family) {
    use sha2::Digest;
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM mirror_backfill_run WHERE scope = $1)")
            .bind(family.scope())
            .fetch_one(&pg.0)
            .await
            .unwrap();
    if exists {
        require_scope(pg, family).await.unwrap();
        return;
    }
    let export = b"synthetic canonical fixture capture";
    let mut counts = std::collections::BTreeMap::new();
    for table in family.tables() {
        let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*) FROM \"{table}\""
        )))
        .fetch_one(&pg.0)
        .await
        .unwrap();
        counts.insert((*table).to_owned(), count);
    }
    let receipt = SourceReceipt {
        format_version: 1,
        consistency: "operator_quiesced".into(),
        export_bytes: u64::try_from(export.len()).unwrap(),
        export_sha256: hex::encode(sha2::Sha256::digest(export)),
        schema_sha256: "ab".repeat(32),
        table_counts: counts,
    };
    receipt.clone().verify_export(export).unwrap();
    receipt.validate(family).unwrap();
    let run = Uuid::new_v4();
    let template_count: i64 =
        sqlx::query_scalar("SELECT count(DISTINCT template_id) FROM notation")
            .fetch_one(pg.pool())
            .await
            .unwrap();
    let mut tx = pg.0.begin().await.unwrap();
    sqlx::query("INSERT INTO mirror_backfill_run (id, batch_size, status, scope, source_receipt, completed_at) VALUES ($1, 1, 'completed', $2, $3, now())")
        .bind(run).bind(family.scope()).bind(serde_json::to_value(&receipt).unwrap()).execute(&mut *tx).await.unwrap();
    for table in family.tables() {
        let count = usize::try_from(receipt.table_counts[*table]).unwrap();
        let proof = Verification {
            source_rows: count,
            target_rows: count,
            differences: 0,
            references_checked: true,
            foreign_keys_checked: true,
            references: vec![],
            reset_facts_checked: family == Family::Notation,
            reset_fact_differences: 0,
            asset_union: (family == Family::Notation && *table == "asset").then_some(
                AssetUnionProof {
                    compared_overlaps: count,
                    full_metadata_compared: true,
                    ..Default::default()
                },
            ),
            reset_union: (family == Family::Notation).then(|| ResetUnionProof {
                compared: usize::try_from(receipt.table_counts["notation"]).unwrap(),
                retained_native_protections: 0,
            }),
            template_pins: (family == Family::Notation).then(|| TemplatePinProof {
                compared: usize::try_from(template_count).unwrap(),
                differences: 0,
            }),
        };
        sqlx::query("INSERT INTO mirror_backfill_checkpoint (run_id, mirror, completed_at, verification) VALUES ($1, $2, now(), $3)")
            .bind(run).bind(checkpoint(table)).bind(serde_json::to_value(proof).unwrap()).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    require_scope(pg, family).await.unwrap();
}

/// Check actual enrolled rows, including constraints marked NOT VALID.
pub(super) async fn foreign_keys_hold(
    pg: &PostgresDb,
    family: Family,
) -> Result<bool, FamilyError> {
    let constraints = sqlx::query(
        "SELECT n.nspname::text AS child_schema, t.relname::text AS child_table, \
         pn.nspname::text AS parent_schema, pt.relname::text AS parent_table, \
         c.confmatchtype::text AS match_type, \
         array_agg(a.attname::text ORDER BY k.ord) AS child_columns, \
         array_agg(pa.attname::text ORDER BY k.ord) AS parent_columns \
         FROM pg_constraint c JOIN pg_class t ON t.oid=c.conrelid \
         JOIN pg_namespace n ON n.oid=t.relnamespace \
         JOIN pg_class pt ON pt.oid=c.confrelid JOIN pg_namespace pn ON pn.oid=pt.relnamespace \
         JOIN LATERAL unnest(c.conkey,c.confkey) WITH ORDINALITY k(child_att,parent_att,ord) ON true \
         JOIN pg_attribute a ON a.attrelid=t.oid AND a.attnum=k.child_att \
         JOIN pg_attribute pa ON pa.attrelid=pt.oid AND pa.attnum=k.parent_att \
         WHERE c.contype='f' AND n.nspname='public' AND t.relname=ANY($1::text[]) \
         GROUP BY c.oid,n.nspname,t.relname,pn.nspname,pt.relname,c.confmatchtype",
    ).bind(family.tables()).fetch_all(&pg.0).await?;
    for row in constraints {
        let children: Vec<String> = row.try_get("child_columns")?;
        let parents: Vec<String> = row.try_get("parent_columns")?;
        if children.is_empty() || children.len() != parents.len() {
            return Err(FamilyError::Receipt);
        }
        let match_type: String = row.try_get("match_type")?;
        let null_join = match match_type.as_str() {
            "s" => " AND ",
            "f" => " OR ",
            _ => return Err(FamilyError::Receipt),
        };
        let applicable = children
            .iter()
            .map(|column| format!("c.{} IS NOT NULL", identifier(column)))
            .collect::<Vec<_>>()
            .join(null_join);
        let equality = children
            .iter()
            .zip(parents)
            .map(|(child, parent)| format!("p.{}=c.{}", identifier(&parent), identifier(child)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let child = format!(
            "{}.{}",
            identifier(&row.try_get::<String, _>("child_schema")?),
            identifier(&row.try_get::<String, _>("child_table")?)
        );
        let parent = format!(
            "{}.{}",
            identifier(&row.try_get::<String, _>("parent_schema")?),
            identifier(&row.try_get::<String, _>("parent_table")?)
        );
        let holds: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT NOT EXISTS (SELECT 1 FROM {child} c WHERE ({applicable}) AND NOT EXISTS (SELECT 1 FROM {parent} p WHERE {equality}))")))
            .fetch_one(&pg.0).await?;
        if !holds {
            return Ok(false);
        }
    }
    Ok(true)
}

fn identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

#[cfg(test)]
#[path = "family_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "notation_family_tests.rs"]
mod notation_tests;
