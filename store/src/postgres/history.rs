//! Verify the completed PostgreSQL enrollment receipts required by existing databases.

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
                "sent_email",
                "journal_entry",
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

/// A scoped import is unavailable or its content-free enrollment evidence is incomplete.
#[derive(Debug, thiserror::Error)]
pub enum FamilyError {
    /// A present fresh-native origin must validate independently of captured history.
    #[error(transparent)]
    NativeOrigin(#[from] crate::postgres::native_origin::OriginError),
    /// Fresh-native authority cannot acquire or replay captured-source history.
    #[error("a fresh-native Database Origin refuses captured-source import")]
    NativeOwnership,
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

fn checkpoint(table: &str) -> String {
    format!("table:{table}")
}

/// Whether fresh-native origin or completed captured history owns this exact table.
///
/// Ownership is looked up by persisted table enrollment, rather than only by the
/// current family version, so a later registry cannot resurrect deleted rows.
///
/// # Errors
/// PostgreSQL could not read the existing backfill ledger.
pub async fn active_for_table(pg: &PostgresDb, table: &str) -> Result<bool, FamilyError> {
    if crate::postgres::native_origin::admits(pg).await? {
        return Ok(true);
    }
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
    if crate::postgres::native_origin::admits(pg).await? {
        return Ok(());
    }
    validated_scope(pg, family).await.map(|_| ())
}

/// Require all registered historical imports before serving application traffic or effects.
/// Database connections and operator import/report commands deliberately do not call this.
///
/// # Errors
/// Any required family is missing, malformed, ambiguous, or PostgreSQL is unavailable.
pub async fn require_runtime_scopes(pg: &PostgresDb) -> Result<(), FamilyError> {
    if crate::postgres::native_origin::admits(pg).await? {
        return Ok(());
    }
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
    if crate::postgres::native_origin::admits(pg).await? {
        return Ok(());
    }
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

/// Exact captured audit coverage, including rows removed by a proven UTC-day purge.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SentEmailUnionProof {
    pub compared_overlaps: usize,
    pub source_only_remaining: usize,
    pub excluded_purged: usize,
    pub differing_overlaps: usize,
    pub native_target_only: usize,
    pub full_metadata_compared: bool,
}

impl SentEmailUnionProof {
    pub(crate) fn valid(&self, source: usize, target: usize, differences: usize) -> bool {
        self.full_metadata_compared
            && self
                .compared_overlaps
                .checked_add(self.source_only_remaining)
                .and_then(|count| count.checked_add(self.excluded_purged))
                == Some(source)
            && self.compared_overlaps.checked_add(self.native_target_only) == Some(target)
            && self.differing_overlaps <= self.compared_overlaps
            && self
                .source_only_remaining
                .checked_add(self.differing_overlaps)
                == Some(differences)
    }
}

/// Content-free reasons an immutable captured Journal history cannot be activated.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum JournalDifferenceReason {
    SourceTimestamp,
    MissingTarget,
    PayloadMismatch,
    HeaderMismatch,
    PostingProjection,
    SourceProjection,
    LegacyProvenance,
    MissingFirm,
    ReversalParent,
    ReversalCycle,
    ReversalNegation,
    DuplicateReversal,
    ActiveMovement,
    MovementEvidence,
    RepaymentShape,
    RepaymentCost,
}

/// Only event/Firm UUIDs leave the proof; keys, payloads and database errors do not.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct JournalDifference {
    pub entry_id: Option<Uuid>,
    pub firm_id: Option<Uuid>,
    pub reason: JournalDifferenceReason,
}

/// Conservation and integrity of the exact captured input and one PostgreSQL snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
struct JournalHandoff {
    pub source_rows: usize,
    pub represented_source_rows: usize,
    pub compared_overlaps: usize,
    pub source_only_remaining: usize,
    pub target_rows: usize,
    pub native_target_only: usize,
    pub unresolved_source_rows: usize,
    pub target_integrity_failures: usize,
    pub differences: Vec<JournalDifference>,
}
impl JournalHandoff {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.compared_overlaps
            .checked_add(self.source_only_remaining)
            == Some(self.source_rows)
            && self.compared_overlaps.checked_add(self.native_target_only) == Some(self.target_rows)
            && self.represented_source_rows <= self.compared_overlaps
            && self
                .represented_source_rows
                .checked_add(self.unresolved_source_rows)
                == Some(self.source_rows)
            && self.source_rows == self.represented_source_rows
            && self.source_only_remaining == 0
            && self.unresolved_source_rows == 0
            && self.target_integrity_failures == 0
            && self.differences.is_empty()
    }
}

fn journal_representable(proof: &JournalHandoff, source: usize, target: usize) -> bool {
    proof.source_rows == source
        && proof.target_rows == target
        && proof
            .compared_overlaps
            .checked_add(proof.source_only_remaining)
            == Some(source)
        && proof
            .compared_overlaps
            .checked_add(proof.native_target_only)
            == Some(target)
        && proof.represented_source_rows <= proof.compared_overlaps
        && proof
            .represented_source_rows
            .checked_add(proof.unresolved_source_rows)
            == Some(source)
        && proof.target_integrity_failures <= target
}

/// Exact Project overlap accounting after validating the complete lifecycle ledger.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectLifecycleProof {
    pub source_only_remaining: usize,
    pub compared_overlaps: usize,
    pub retained_native_projections: usize,
    pub unresolved_overlaps: usize,
    pub target_only: usize,
    pub full_metadata_compared: bool,
    pub ledger_projections_checked: bool,
}

impl ProjectLifecycleProof {
    fn valid(&self, source: usize, target: usize, differences: usize) -> bool {
        self.full_metadata_compared
            && self.ledger_projections_checked
            && self
                .source_only_remaining
                .checked_add(self.compared_overlaps)
                == Some(source)
            && self.compared_overlaps.checked_add(self.target_only) == Some(target)
            && self
                .retained_native_projections
                .checked_add(self.unresolved_overlaps)
                .is_some_and(|count| count <= self.compared_overlaps)
            && self
                .source_only_remaining
                .checked_add(self.unresolved_overlaps)
                .and_then(|count| count.checked_add(self.target_only))
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_lifecycle: Option<ProjectLifecycleProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sent_email_union: Option<SentEmailUnionProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    journal_handoff: Option<JournalHandoff>,
}

impl Verification {
    fn clean(&self, family: Family, captured: usize, table: &str) -> bool {
        self.source_rows == captured
            && if family == Family::Notation && table == "asset" {
                self.asset_union
                    .as_ref()
                    .is_some_and(|proof| proof.valid(captured, self.target_rows, self.differences))
            } else if family == Family::Application && table == "sent_email" {
                self.asset_union.is_none()
                    && self.sent_email_union.as_ref().is_some_and(|proof| {
                        proof.valid(captured, self.target_rows, self.differences)
                    })
            } else if family == Family::Application && table == "journal_entry" {
                self.asset_union.is_none()
                    && self.journal_handoff.as_ref().is_some_and(|proof| {
                        journal_representable(proof, captured, self.target_rows) && proof.is_clean()
                    })
            } else {
                self.target_rows == captured && self.asset_union.is_none()
            }
            && (family == Family::Application && table == "sent_email"
                || self.sent_email_union.is_none())
            && (family == Family::Application && table == "journal_entry"
                || self.journal_handoff.is_none())
            && match family {
                Family::Satellites | Family::Application | Family::Identity => {
                    self.reset_union.is_none()
                        && self.template_pins.is_none()
                        && self.project_lifecycle.is_none()
                }
                Family::Tenancy => {
                    self.reset_union.is_none()
                        && self.template_pins.is_none()
                        && if table == "project" {
                            self.project_lifecycle.as_ref().is_some_and(|proof| {
                                proof.valid(captured, self.target_rows, self.differences)
                            })
                        } else {
                            self.project_lifecycle.is_none()
                        }
                }
                Family::Notation => {
                    self.project_lifecycle.is_none()
                        && self.reset_facts_checked
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
                reference_column(table, &reference.field)
                    && reference.source_orphans <= captured
                    && reference.postgres_missing <= captured
                    && reference.newly_target_missing == 0
            })
    }
}

/// A completed global enrollment is required for a database without a native origin.
pub async fn global_completed(pg: &PostgresDb) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM mirror_backfill_run WHERE scope = 'global' AND status = 'completed')",
    )
    .fetch_one(pg.pool())
    .await
}

// Field names accepted in persisted enrollment receipts.
const REFERENCE_COLUMNS: &[(&str, &[&str])] = &[
    ("address", &["entity_id", "person_id"]),
    (
        "asset",
        &[
            "deleted_by_person_id",
            "project_id",
            "secondary_storage_key",
            "storage_key",
        ],
    ),
    ("authority_use", &["authority_id", "project_id"]),
    ("brand", &["firm_id"]),
    ("cap_table_holding", &["holder_id", "project_id"]),
    ("case", &["project_id"]),
    (
        "case_docket_entry",
        &["case_id", "document_asset_id", "notation_id", "supersedes"],
    ),
    ("citation", &["authority_use_id"]),
    (
        "contract_review",
        &["asset_id", "notation_id", "playbook_id"],
    ),
    ("credential", &["jurisdiction_id", "person_id"]),
    ("disclosure", &["entity_id", "project_id"]),
    ("discovery_item", &["discovery_request_id"]),
    ("discovery_request", &["case_id", "docket_entry_id"]),
    ("document_comment", &["person_id", "review_document_id"]),
    (
        "efiling_transactions",
        &[
            "conformed_document_id",
            "project_id",
            "submitted_document_id",
        ],
    ),
    ("email_token", &["person_id"]),
    ("entity", &["entity_type_id", "jurisdiction_id"]),
    ("estate_asset", &["project_id"]),
    ("expunge_record", &["authorized_by_person_id", "project_id"]),
    (
        "expunge_request",
        &[
            "asset_id",
            "expunge_record_id",
            "project_id",
            "requested_by_person_id",
            "resolved_by_person_id",
        ],
    ),
    (
        "fiduciary_role",
        &["fiduciary_id", "notation_id", "project_id"],
    ),
    ("filing", &["notation_id"]),
    ("finance_aggregate", &["project_id"]),
    ("firm", &["entity_id"]),
    ("firm_anchor", &["entity_id"]),
    ("firm_brand", &["firm_id"]),
    ("firm_integration_secret", &["actor_id", "firm_id"]),
    ("git_access_token", &["person_id", "project_id"]),
    (
        "inbox_triage",
        &["firm_id", "message_id", "suggested_project_id"],
    ),
    ("iolta_account", &["jurisdiction_id"]),
    (
        "iolta_allocation",
        &["project_id", "withdrawal_id", "xero_invoice_id"],
    ),
    ("iolta_withdrawal", &["jurisdiction_id"]),
    ("lead", &["person_id"]),
    ("lead_consent", &["lead_id"]),
    ("letter", &["mailroom_id", "project_id"]),
    ("mailroom", &["address_id"]),
    ("matter_page", &["firm_id", "project_id"]),
    (
        "notarization",
        &["asset_id", "notary_person_id", "notation_id"],
    ),
    (
        "notation_document",
        &["current_version_id", "notation_id", "original_asset_id"],
    ),
    ("notation_document_comment", &["person_id", "version_id"]),
    (
        "notation_document_version",
        &[
            "anchor_manifest_asset_id",
            "authored_by_person_id",
            "document_id",
            "markdown_asset_id",
            "parent_version_id",
        ],
    ),
    ("notation_draft", &["notation_id", "project_id"]),
    (
        "notation_meter_entry",
        &["notation_id", "package_id", "project_id", "rate_id"],
    ),
    ("notation_source", &["communication_id", "notation_id"]),
    (
        "person_delegate",
        &[
            "delegate_person_id",
            "granted_by_person_id",
            "revoked_by_person_id",
            "subject_person_id",
        ],
    ),
    ("person_external_identity", &["person_id"]),
    ("person_firm_role", &["firm_id", "person_id"]),
    ("person_mailbox", &["person_id"]),
    ("person_phone", &["person_id"]),
    ("person_project_role", &["person_id", "project_id"]),
    ("playbook", &["entity_id"]),
    (
        "project",
        &[
            "client_dri_person_id",
            "entity_id",
            "firm_id",
            "jurisdiction_id",
            "legal_plan_id",
        ],
    ),
    (
        "project_deadlines",
        &["owner_person_id", "project_id", "source_document_id"],
    ),
    ("project_module", &["enabled_by_person_id", "project_id"]),
    (
        "project_service",
        &[
            "declarant_person_id",
            "document_id",
            "letter_id",
            "project_id",
            "recipient_entity_id",
            "recipient_person_id",
            "sent_email_id",
        ],
    ),
    ("relationship", &["in", "out", "source_id"]),
    ("review_document", &["notation_id"]),
    ("sent_email", &["project_id"]),
    ("signature", &["notation_id", "signer_person_id"]),
    ("social_oauth_attempt", &["actor_person_id", "firm_id"]),
    ("statutory_deadline", &["project_id"]),
    (
        "summary_sender_approval",
        &["firm_id", "person_id", "project_id"],
    ),
    (
        "summary_sender_approval_event",
        &["actor_person_id", "firm_id", "person_id", "project_id"],
    ),
    ("template", &["asset_id", "firm_id", "project_id"]),
    ("testimonial", &["person_id", "project_id"]),
    ("verification", &["citation_id", "verifier_person_id"]),
    ("xero_invoice", &["project_id"]),
];

fn reference_column(table: &str, field: &str) -> bool {
    REFERENCE_COLUMNS
        .iter()
        .any(|(name, columns)| *name == table && columns.contains(&field))
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
        let proof = fixture_verification(family, table, count, &receipt, template_count);
        sqlx::query("INSERT INTO mirror_backfill_checkpoint (run_id, mirror, completed_at, verification) VALUES ($1, $2, now(), $3)")
            .bind(run).bind(checkpoint(table)).bind(serde_json::to_value(proof).unwrap()).execute(&mut *tx).await.unwrap();
    }
    tx.commit().await.unwrap();
    require_scope(pg, family).await.unwrap();
}

#[cfg(any(test, feature = "test-support"))]
fn fixture_verification(
    family: Family,
    table: &str,
    count: usize,
    receipt: &SourceReceipt,
    template_count: i64,
) -> Verification {
    Verification {
        source_rows: count,
        target_rows: count,
        differences: 0,
        references_checked: true,
        foreign_keys_checked: true,
        references: vec![],
        reset_facts_checked: family == Family::Notation,
        reset_fact_differences: 0,
        asset_union: (family == Family::Notation && table == "asset").then_some(AssetUnionProof {
            compared_overlaps: count,
            full_metadata_compared: true,
            ..Default::default()
        }),
        reset_union: (family == Family::Notation).then(|| ResetUnionProof {
            compared: usize::try_from(receipt.table_counts["notation"]).unwrap(),
            retained_native_protections: 0,
        }),
        template_pins: (family == Family::Notation).then(|| TemplatePinProof {
            compared: usize::try_from(template_count).unwrap(),
            differences: 0,
        }),
        project_lifecycle: (family == Family::Tenancy && table == "project").then_some(
            ProjectLifecycleProof {
                compared_overlaps: count,
                full_metadata_compared: true,
                ledger_projections_checked: true,
                ..Default::default()
            },
        ),
        sent_email_union: (family == Family::Application && table == "sent_email").then_some(
            SentEmailUnionProof {
                compared_overlaps: count,
                full_metadata_compared: true,
                ..Default::default()
            },
        ),
        journal_handoff: if family == Family::Application && table == "journal_entry" {
            assert_eq!(
                count, 0,
                "Journal fixture enrollment precedes native records"
            );
            Some(JournalHandoff::default())
        } else {
            None
        },
    }
}
