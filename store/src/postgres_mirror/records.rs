//! Whole-row mirror of tables whose PostgreSQL row is the SurrealDB row with
//! its record links as UUID foreign keys.
//!
//! Each [`Table`] names its columns, which of them are record links and the
//! table each link names, and which carry timestamps. A row keeps its record
//! UUID as `id`. A source field without a declared preservation mapping is refused.
//! Parity compares whole rows as JSON, timestamps at
//! PostgreSQL's microsecond precision. The upsert writes only when a column
//! differs, so a replay changes nothing. A row SurrealDB no longer has is
//! removed from PostgreSQL.
//!
//! The tables form named groups, one [`super::MIRRORS`] entry each:
//!
//! - `project_records`: Project modules, statutory deadlines, testimonials,
//!   and the client Entity's playbooks.
//! - `notation_inputs`: a Notation's ordered sources and a Project's unrun
//!   Notation drafts.
//! - `git_access`: tracked Git repositories and Git access tokens, stored as
//!   their SHA-256 hash only.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use super::{changed, Columns, MirrorError, MirrorKey, Mirrored, ParityReport};
use crate::{postgres::PostgresDb, surreal::SurrealDb};

/// One mirrored table. Its PostgreSQL columns are exactly `columns`.
pub(super) struct Table {
    pub(super) name: &'static str,
    group: &'static str,
    columns: &'static [&'static str],
    /// Record-link columns and the table each one names.
    references: &'static [(&'static str, &'static str)],
    /// Columns PostgreSQL stores as `timestamptz`.
    timestamps: &'static [&'static str],
}

pub(super) const TABLES: &[Table] = &[
    Table {
        name: "authority_use",
        group: "source_domain_records",
        columns: &[
            "id",
            "authority_id",
            "disposition",
            "inserted_at",
            "position",
            "project_id",
            "role",
            "updated_at",
        ],
        references: &[("authority_id", "authority"), ("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "cap_table_holding",
        group: "source_domain_records",
        columns: &[
            "id",
            "holder_id",
            "inserted_at",
            "project_id",
            "share_class",
            "shares",
            "updated_at",
            "vesting_note",
        ],
        references: &[("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "case",
        group: "source_domain_records",
        columns: &[
            "id",
            "caption",
            "docket_number",
            "forum",
            "inserted_at",
            "judge",
            "jurisdiction",
            "posture",
            "project_id",
            "status",
            "updated_at",
        ],
        references: &[("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "case_docket_entry",
        group: "source_domain_records",
        columns: &[
            "id",
            "case_id",
            "document_asset_id",
            "entry_number",
            "filed_or_served_on",
            "inserted_at",
            "integrity_checked_at",
            "integrity_matched",
            "integrity_source_sha256_hex",
            "kind",
            "notation_id",
            "party",
            "scheduled_on",
            "stamp_case_number",
            "stamp_entry_number",
            "stamp_filed_on",
            "supersedes",
            "title",
            "updated_at",
        ],
        references: &[
            ("case_id", "case"),
            ("document_asset_id", "asset"),
            ("notation_id", "notation"),
            ("supersedes", "case_docket_entry"),
        ],
        timestamps: &[
            "inserted_at",
            "integrity_checked_at",
            "scheduled_on",
            "updated_at",
        ],
    },
    Table {
        name: "citation",
        group: "source_domain_records",
        columns: &[
            "id",
            "authority_use_id",
            "draft_pin",
            "inserted_at",
            "quote",
            "source_pin",
            "updated_at",
            "why",
        ],
        references: &[("authority_use_id", "authority_use")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "contract_review",
        group: "source_domain_records",
        columns: &[
            "id",
            "asset_id",
            "findings",
            "inserted_at",
            "notation_id",
            "playbook_id",
            "risk_summary",
            "status",
            "updated_at",
        ],
        references: &[
            ("asset_id", "asset"),
            ("notation_id", "notation"),
            ("playbook_id", "playbook"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "deployment_secret_change",
        group: "source_domain_records",
        columns: &[
            "id",
            "actor_id",
            "ciphertext",
            "inserted_at",
            "key",
            "kms_key_version",
            "request_id",
            "updated_at",
            "wrapped_key",
        ],
        references: &[],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "disclosure",
        group: "source_domain_records",
        columns: &[
            "id",
            "entity_id",
            "inserted_at",
            "kind",
            "project_id",
            "summary",
            "updated_at",
        ],
        references: &[("entity_id", "entity"), ("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "discovery_item",
        group: "source_domain_records",
        columns: &[
            "id",
            "discovery_request_id",
            "inserted_at",
            "item_number",
            "objections",
            "request_text",
            "response_text",
            "updated_at",
        ],
        references: &[("discovery_request_id", "discovery_request")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "discovery_request",
        group: "source_domain_records",
        columns: &[
            "id",
            "case_id",
            "device",
            "docket_entry_id",
            "inserted_at",
            "propounding_party",
            "responding_party",
            "responses_due_on",
            "served_on",
            "set_number",
            "status",
            "updated_at",
        ],
        references: &[
            ("case_id", "case"),
            ("docket_entry_id", "case_docket_entry"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "document_comment",
        group: "source_domain_records",
        columns: &[
            "id",
            "anchor_end",
            "anchor_start",
            "body",
            "communication_id",
            "inserted_at",
            "person_id",
            "quoted_text",
            "resolved",
            "review_document_id",
            "updated_at",
        ],
        references: &[
            ("person_id", "person"),
            ("review_document_id", "review_document"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "efiling_transactions",
        group: "source_domain_records",
        columns: &[
            "id",
            "accepted_on",
            "case_number",
            "conformed_document_id",
            "court",
            "efsp",
            "filed_on",
            "inserted_at",
            "needs_entered_order",
            "note",
            "project_id",
            "status",
            "submitted_document_id",
            "transaction_id",
            "updated_at",
        ],
        references: &[
            ("conformed_document_id", "asset"),
            ("project_id", "project"),
            ("submitted_document_id", "asset"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "estate_asset",
        group: "source_domain_records",
        columns: &[
            "id",
            "category",
            "description",
            "funding_state",
            "inserted_at",
            "note",
            "passes_by",
            "project_id",
            "updated_at",
        ],
        references: &[("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "expunge_record",
        group: "source_domain_records",
        columns: &[
            "id",
            "authorized_by_person_id",
            "category",
            "head_after",
            "head_before",
            "inserted_at",
            "note",
            "path",
            "project_id",
            "updated_at",
        ],
        references: &[
            ("authorized_by_person_id", "person"),
            ("project_id", "project"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "expunge_request",
        group: "source_domain_records",
        columns: &[
            "id",
            "asset_id",
            "expunge_record_id",
            "inserted_at",
            "note",
            "project_id",
            "requested_by_person_id",
            "resolved_by_person_id",
            "status",
            "updated_at",
        ],
        references: &[
            ("asset_id", "asset"),
            ("expunge_record_id", "expunge_record"),
            ("project_id", "project"),
            ("requested_by_person_id", "person"),
            ("resolved_by_person_id", "person"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "fiduciary_role",
        group: "source_domain_records",
        columns: &[
            "id",
            "fiduciary_id",
            "inserted_at",
            "notation_id",
            "project_id",
            "role",
            "successor_order",
            "updated_at",
        ],
        references: &[("notation_id", "notation"), ("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "filing",
        group: "source_domain_records",
        columns: &[
            "id",
            "inserted_at",
            "kind",
            "notation_id",
            "office",
            "reference",
            "submitted_at",
            "summary",
            "updated_at",
        ],
        references: &[("notation_id", "notation")],
        timestamps: &["inserted_at", "submitted_at", "updated_at"],
    },
    Table {
        name: "finance_aggregate",
        group: "source_domain_records",
        columns: &[
            "id",
            "computed_at",
            "inserted_at",
            "parameters",
            "project_id",
            "report_json",
            "run_id",
            "updated_at",
        ],
        references: &[("project_id", "project")],
        timestamps: &["computed_at", "inserted_at", "updated_at"],
    },
    Table {
        name: "firm_anchor",
        group: "source_domain_records",
        columns: &["id", "id_kind", "entity_id"],
        references: &[],
        timestamps: &[],
    },
    Table {
        name: "iolta_account",
        group: "source_domain_records",
        columns: &[
            "id",
            "balance_cents",
            "currency",
            "inserted_at",
            "jurisdiction_id",
            "mirrored_at",
            "name",
            "updated_at",
            "xero_account_code",
            "xero_account_id",
        ],
        references: &[("jurisdiction_id", "jurisdiction")],
        timestamps: &["inserted_at", "mirrored_at", "updated_at"],
    },
    Table {
        name: "iolta_allocation",
        group: "source_domain_records",
        columns: &[
            "id",
            "amount_cents",
            "inserted_at",
            "invoice_reference",
            "occurred_at",
            "project_id",
            "updated_at",
            "withdrawal_id",
            "xero_invoice_id",
        ],
        references: &[("project_id", "project")],
        timestamps: &["inserted_at", "occurred_at", "updated_at"],
    },
    Table {
        name: "iolta_withdrawal",
        group: "source_domain_records",
        columns: &[
            "id",
            "currency",
            "inserted_at",
            "jurisdiction_id",
            "occurred_at",
            "total_cents",
            "updated_at",
            "xero_transaction_id",
        ],
        references: &[("jurisdiction_id", "jurisdiction")],
        timestamps: &["inserted_at", "occurred_at", "updated_at"],
    },
    Table {
        name: "notarization",
        group: "source_domain_records",
        columns: &[
            "id",
            "asset_id",
            "inserted_at",
            "notarized_at",
            "notary_person_id",
            "notation_id",
            "provider",
            "provider_id",
            "updated_at",
        ],
        references: &[
            ("asset_id", "asset"),
            ("notary_person_id", "person"),
            ("notation_id", "notation"),
        ],
        timestamps: &["inserted_at", "notarized_at", "updated_at"],
    },
    Table {
        name: "notation_document",
        group: "source_domain_records",
        columns: &[
            "id",
            "current_version_id",
            "inserted_at",
            "notation_id",
            "original_asset_id",
            "updated_at",
        ],
        references: &[
            ("current_version_id", "notation_document_version"),
            ("notation_id", "notation"),
            ("original_asset_id", "asset"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "notation_document_comment",
        group: "source_domain_records",
        columns: &[
            "id",
            "anchor",
            "body",
            "decision",
            "inserted_at",
            "person_id",
            "resolved",
            "updated_at",
            "version_id",
        ],
        references: &[
            ("person_id", "person"),
            ("version_id", "notation_document_version"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "notation_document_version",
        group: "source_domain_records",
        columns: &[
            "id",
            "anchor_manifest_asset_id",
            "authored_by_person_id",
            "document_id",
            "inserted_at",
            "markdown_asset_id",
            "parent_version_id",
            "parser_version",
            "schema_version",
            "status",
            "updated_at",
        ],
        references: &[
            ("anchor_manifest_asset_id", "asset"),
            ("authored_by_person_id", "person"),
            ("document_id", "notation_document"),
            ("markdown_asset_id", "asset"),
            ("parent_version_id", "notation_document_version"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "notation_meter_entry",
        group: "source_domain_records",
        columns: &[
            "id",
            "currency",
            "inserted_at",
            "notation_id",
            "package_id",
            "project_id",
            "quantity",
            "rate_id",
            "state",
            "unit_amount_cents",
            "updated_at",
        ],
        references: &[
            ("notation_id", "notation"),
            ("package_id", "notation_package"),
            ("project_id", "project"),
            ("rate_id", "notation_rate"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "person_phone",
        group: "source_domain_records",
        columns: &[
            "id",
            "e164",
            "inserted_at",
            "is_primary",
            "label",
            "person_id",
            "updated_at",
        ],
        references: &[("person_id", "person")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "relationship",
        group: "source_domain_records",
        columns: &[
            "id",
            "confidence_pct",
            "detail",
            "inserted_at",
            "kind",
            "source_id",
            "source_kind",
            "updated_at",
            "in",
            "out",
        ],
        references: &[],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "review_document",
        group: "source_domain_records",
        columns: &[
            "id",
            "body_html",
            "inserted_at",
            "kind",
            "notation_id",
            "status",
            "title",
            "updated_at",
        ],
        references: &[("notation_id", "notation")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "signature",
        group: "source_domain_records",
        columns: &[
            "id",
            "completed_signer_roles",
            "field",
            "inserted_at",
            "notation_id",
            "provider",
            "provider_id",
            "signed_at",
            "signer_person_id",
            "state",
            "updated_at",
        ],
        references: &[("notation_id", "notation"), ("signer_person_id", "person")],
        timestamps: &["inserted_at", "signed_at", "updated_at"],
    },
    Table {
        name: "social_oauth_attempt",
        group: "source_domain_records",
        columns: &[
            "id",
            "actor_person_id",
            "brand_key",
            "consumed_at",
            "expires_at",
            "firm_id",
            "inserted_at",
            "instagram_account_id",
            "pkce_verifier",
            "provider",
            "provider_account_id",
            "state_hash",
        ],
        references: &[("actor_person_id", "person"), ("firm_id", "firm")],
        timestamps: &["consumed_at", "expires_at", "inserted_at"],
    },
    Table {
        name: "verification",
        group: "source_domain_records",
        columns: &[
            "id",
            "citation_id",
            "inserted_at",
            "revision_sha",
            "status_citation",
            "status_proposition",
            "status_quote",
            "updated_at",
            "verifier_person_id",
        ],
        references: &[
            ("citation_id", "citation"),
            ("verifier_person_id", "person"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "visitor_route_count",
        group: "source_domain_records",
        columns: &[
            "id",
            "bucket_date",
            "country_code",
            "inserted_at",
            "locale",
            "route_pattern",
            "source",
            "status_class",
            "updated_at",
            "visits",
        ],
        references: &[],
        timestamps: &["bucket_date", "inserted_at", "updated_at"],
    },
    Table {
        name: "xero_invoice",
        group: "source_domain_records",
        columns: &[
            "id",
            "amount_cents",
            "amount_paid_cents",
            "currency",
            "due_at",
            "inserted_at",
            "issued_at",
            "project_id",
            "reference",
            "status",
            "updated_at",
            "xero_invoice_id",
        ],
        references: &[("project_id", "project")],
        timestamps: &["due_at", "inserted_at", "issued_at", "updated_at"],
    },
    Table {
        name: "project_module",
        group: "project_records",
        columns: &[
            "id",
            "project_id",
            "module",
            "enabled_at",
            "enabled_by_person_id",
            "inserted_at",
            "updated_at",
        ],
        references: &[
            ("project_id", "project"),
            ("enabled_by_person_id", "person"),
        ],
        timestamps: &["enabled_at", "inserted_at", "updated_at"],
    },
    Table {
        name: "statutory_deadline",
        group: "project_records",
        columns: &[
            "id",
            "project_id",
            "kind",
            "trigger_on",
            "due_on",
            "statute",
            "source",
            "status",
            "inserted_at",
            "updated_at",
        ],
        references: &[("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "testimonial",
        group: "project_records",
        columns: &[
            "id",
            "project_id",
            "person_id",
            "quote",
            "attribution_label",
            "consented_at",
            "published_at",
            "display_order",
            "inserted_at",
            "updated_at",
        ],
        references: &[("project_id", "project"), ("person_id", "person")],
        timestamps: &["consented_at", "published_at", "inserted_at", "updated_at"],
    },
    Table {
        name: "playbook",
        group: "project_records",
        columns: &[
            "id",
            "entity_id",
            "name",
            "positions",
            "active",
            "inserted_at",
            "updated_at",
        ],
        references: &[("entity_id", "entity")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "notation_source",
        group: "notation_inputs",
        columns: &[
            "id",
            "notation_id",
            "kind",
            "communication_id",
            "source_ref",
            "position",
            "inserted_at",
            "updated_at",
        ],
        references: &[
            ("notation_id", "notation"),
            ("communication_id", "communication"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "notation_draft",
        group: "notation_inputs",
        columns: &[
            "id",
            "project_id",
            "slug",
            "title",
            "source",
            "notation_id",
            "inserted_at",
            "expires_at",
        ],
        references: &[("project_id", "project"), ("notation_id", "notation")],
        timestamps: &["inserted_at", "expires_at"],
    },
    Table {
        name: "git_repository",
        group: "git_access",
        columns: &[
            "id",
            "remote_hash",
            "last_commit_sha",
            "inserted_at",
            "updated_at",
        ],
        references: &[],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "git_access_token",
        group: "git_access",
        columns: &[
            "id",
            "person_id",
            "project_id",
            "token_hash",
            "scope",
            "expires_at",
            "inserted_at",
            "updated_at",
        ],
        references: &[("person_id", "person"), ("project_id", "project")],
        timestamps: &["expires_at", "inserted_at", "updated_at"],
    },
];

/// Surreal record tables whose primary key is a domain-provided string.
const STRING_KEY_TABLES: &[&str] = &[
    "finance_aggregate",
    "iolta_account",
    "iolta_allocation",
    "iolta_withdrawal",
    "relationship",
    "xero_invoice",
];
const MIXED_KEY_TABLES: &[&str] = &["firm_anchor"];

const TAGGED_REFERENCES: &[(&str, &str, &[&str])] = &[
    ("cap_table_holding", "holder_id", &["person", "entity"]),
    ("fiduciary_role", "fiduciary_id", &["person", "entity"]),
    (
        "relationship",
        "source_id",
        &["relationship_log", "disclosure"],
    ),
    ("relationship", "in", &["person", "entity"]),
    ("relationship", "out", &["person", "entity"]),
    ("firm_anchor", "entity_id", &["entity"]),
];
const NATURAL_REFERENCES: &[(&str, &str, &str)] = &[
    ("iolta_allocation", "withdrawal_id", "iolta_withdrawal"),
    ("iolta_allocation", "xero_invoice_id", "xero_invoice"),
];

/// A source record link whose target record does not exist in SurrealDB.
/// Only the table, field, and count are exposed; row values remain private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedReference {
    pub table: &'static str,
    pub field: &'static str,
    pub source_orphans: usize,
    pub postgres_missing: usize,
}

/// Count dangling concrete and tagged record links without rewriting them.
#[allow(clippy::too_many_lines)] // Each declared reference family shares one fail-closed inventory pass.
pub async fn unresolved_references(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Vec<UnresolvedReference>, MirrorError> {
    let mut parent_tables = std::collections::BTreeSet::new();
    for table in TABLES {
        for (_, parent) in table.references {
            parent_tables.insert(*parent);
        }
    }
    for (_, _, parents) in TAGGED_REFERENCES {
        parent_tables.extend(*parents);
    }

    let mut natural_parent_tables = std::collections::BTreeSet::new();
    for (_, _, parent) in NATURAL_REFERENCES {
        natural_parent_tables.insert(*parent);
    }

    let mut parent_ids = BTreeMap::<&str, std::collections::BTreeSet<Uuid>>::new();
    let mut target_parent_ids = BTreeMap::<&str, std::collections::BTreeSet<Uuid>>::new();
    for parent in parent_tables {
        let ids = super::select_table(surreal, parent, format!("SELECT VALUE id FROM `{parent}`"))
            .await?;
        let mut decoded_ids = std::collections::BTreeSet::new();
        for id in ids {
            decoded_ids.insert(uuid_in(&id.into_json_value()).ok_or(MirrorError::Source)?);
        }
        parent_ids.insert(parent, decoded_ids);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id FROM \"{parent}\"")))
            .fetch_all(&pg.0)
            .await?;
        target_parent_ids.insert(
            parent,
            rows.iter()
                .map(|row| row.try_get::<Uuid, _>("id"))
                .collect::<Result<_, _>>()?,
        );
    }
    let mut source_natural_ids = BTreeMap::<&str, std::collections::BTreeSet<String>>::new();
    let mut target_natural_ids = BTreeMap::<&str, std::collections::BTreeSet<String>>::new();
    for parent in natural_parent_tables {
        let ids = super::select_table(surreal, parent, format!("SELECT VALUE id FROM `{parent}`"))
            .await?;
        let parent_table = table(parent)?;
        let mut decoded_ids = std::collections::BTreeSet::new();
        for id in ids {
            decoded_ids.insert(string_record_key(parent_table, &id).ok_or(MirrorError::Source)?);
        }
        source_natural_ids.insert(parent, decoded_ids);
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT id FROM \"{parent}\"")))
            .fetch_all(&pg.0)
            .await?;
        target_natural_ids.insert(
            parent,
            rows.iter()
                .map(|row| row.try_get::<String, _>("id"))
                .collect::<Result<_, _>>()?,
        );
    }

    let mut counts = BTreeMap::<(&'static str, &'static str), (usize, usize)>::new();
    for table in TABLES {
        let rows = source(surreal, table).await?;
        for (record_key, row) in &rows {
            for (source_table, field, parent) in NATURAL_REFERENCES
                .iter()
                .filter(|(source_table, _, _)| *source_table == table.name)
            {
                let Some(value) = row.get(*field).and_then(Value::as_str) else {
                    continue;
                };
                let source_orphan = !source_natural_ids
                    .get(parent)
                    .is_some_and(|ids| ids.contains(value));
                let target_missing = !target_natural_ids
                    .get(parent)
                    .is_some_and(|ids| ids.contains(value));
                if source_orphan || target_missing {
                    let status = counts.entry((*source_table, *field)).or_default();
                    status.0 += usize::from(source_orphan);
                    status.1 += usize::from(target_missing);
                }
            }
            for (field, parent) in table.references {
                let Some(value) = row.get(*field).filter(|value| !value.is_null()) else {
                    continue;
                };
                note_reference(
                    &mut counts,
                    (table.name, field),
                    uuid_in(value),
                    parent,
                    &parent_ids,
                    &target_parent_ids,
                );
            }
            for (source_table, field, parents) in TAGGED_REFERENCES
                .iter()
                .filter(|(source_table, _, _)| *source_table == table.name)
            {
                let Some(value) = row.get(*field).filter(|value| !value.is_null()) else {
                    continue;
                };
                if is_firm_anchor_sentinel(record_key, value) {
                    continue;
                }
                let tagged_parent = value.get("tb").and_then(Value::as_str).or_else(|| {
                    value
                        .as_str()
                        .and_then(|text| text.split_once(':').map(|(table, _)| table))
                });
                if let Some(parent) = tagged_parent.filter(|parent| parents.contains(parent)) {
                    note_reference(
                        &mut counts,
                        (*source_table, *field),
                        uuid_in(value),
                        parent,
                        &parent_ids,
                        &target_parent_ids,
                    );
                } else {
                    let status = counts.entry((*source_table, *field)).or_default();
                    status.0 += 1;
                    status.1 += 1;
                }
            }
        }
    }
    Ok(counts
        .into_iter()
        .map(
            |((table, field), (source_orphans, postgres_missing))| UnresolvedReference {
                table,
                field,
                source_orphans,
                postgres_missing,
            },
        )
        .collect())
}

fn is_firm_anchor_sentinel(key: &MirrorKey, value: &Value) -> bool {
    matches!(
        key,
        MirrorKey::Record {
            table: "firm_anchor",
            id
        } if id.is_nil()
    ) && value.get("tb").and_then(Value::as_str) == Some("entity")
        && value
            .get("id")
            .and_then(|id| id.get("String"))
            .and_then(Value::as_str)
            == Some("__navigator_claim_sentinel__")
}

fn note_reference(
    counts: &mut BTreeMap<(&'static str, &'static str), (usize, usize)>,
    key: (&'static str, &'static str),
    id: Option<Uuid>,
    parent: &str,
    source: &BTreeMap<&str, std::collections::BTreeSet<Uuid>>,
    target: &BTreeMap<&str, std::collections::BTreeSet<Uuid>>,
) {
    let source_orphan =
        id.is_none_or(|id| !source.get(parent).is_some_and(|ids| ids.contains(&id)));
    let target_missing =
        id.is_none_or(|id| !target.get(parent).is_some_and(|ids| ids.contains(&id)));
    if source_orphan || target_missing {
        let status = counts.entry(key).or_default();
        status.0 += usize::from(source_orphan);
        status.1 += usize::from(target_missing);
    }
}

/// The registered table of this name.
pub(super) fn table(name: &str) -> Result<&'static Table, MirrorError> {
    TABLES
        .iter()
        .find(|table| table.name == name)
        .ok_or(MirrorError::Source)
}

/// The mirror key of one row of a registered table.
///
/// # Errors
///
/// [`MirrorError::Source`] when no registered table has this name.
pub fn key(table_name: &str, id: Uuid) -> Result<MirrorKey, MirrorError> {
    Ok(MirrorKey::Record {
        table: table(table_name)?.name,
        id,
    })
}

/// Mirror one row of a registered table after its SurrealDB write succeeded.
/// Like [`super::after_write`], it never fails the writer.
pub async fn after_write(surreal: &SurrealDb, table_name: &str, id: Uuid) {
    if let Ok(key) = key(table_name, id) {
        super::after_write(surreal, key).await;
    } else {
        tracing::error!(table = table_name, "no PostgreSQL mirror for this table");
    }
}

fn uuid_in(value: &Value) -> Option<Uuid> {
    match value {
        Value::String(text) => {
            let text = text.rsplit(':').next()?.trim_matches(['⟨', '⟩', '"']);
            let text = text
                .strip_prefix("u'")
                .and_then(|text| text.strip_suffix('\''))
                .unwrap_or(text);
            Uuid::parse_str(text).ok()
        }
        Value::Object(fields) => fields.values().find_map(uuid_in),
        _ => None,
    }
}

fn string_key_in(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => (!text.is_empty()).then(|| text.clone()),
        Value::Object(fields) => fields.values().find_map(string_key_in),
        _ => None,
    }
}

fn normalize_surreal(
    table: &'static Table,
    row: surrealdb::types::Value,
) -> Result<(MirrorKey, Value), MirrorError> {
    let mut used_identity_alias = false;
    let natural_id = if STRING_KEY_TABLES.contains(&table.name)
        || MIXED_KEY_TABLES.contains(&table.name)
    {
        match &row {
            surrealdb::types::Value::Object(fields) => {
                let id = fields.get("id").ok_or(MirrorError::Source)?;
                let raw_string_key = string_record_key(table, id).or_else(|| {
                    fields.get("__surreal_key").and_then(|key| match key {
                        surrealdb::types::Value::String(key) => {
                            used_identity_alias = true;
                            Some(key.clone())
                        }
                        _ => None,
                    })
                });
                if let Some(id) = raw_string_key {
                    Some((id, "string"))
                } else if MIXED_KEY_TABLES.contains(&table.name) {
                    let surrealdb::types::Value::RecordId(record_id) = id else {
                        return Err(MirrorError::Source);
                    };
                    if record_id.table.as_str() != table.name {
                        return Err(MirrorError::Source);
                    }
                    match &record_id.key {
                        surrealdb::types::RecordIdKey::Uuid(id) => Some((id.to_string(), "uuid")),
                        _ => return Err(MirrorError::Source),
                    }
                } else {
                    return Err(MirrorError::Source);
                }
            }
            _ => return Err(MirrorError::Source),
        }
    } else {
        None
    };
    if let surrealdb::types::Value::Object(fields) = &row {
        let count = fields
            .keys()
            .filter(|name| {
                // id_kind is generated from the typed source key, not copied from a source field.
                let mapped = table.columns.contains(&name.as_str())
                    && !(name.as_str() == "id_kind" && MIXED_KEY_TABLES.contains(&table.name));
                !(mapped || used_identity_alias && name.as_str() == "__surreal_key")
            })
            .count();
        if count > 0 {
            return Err(MirrorError::UnmappedSourceFields {
                table: table.name,
                count,
            });
        }
    }
    let mut tagged_values = BTreeMap::<&str, Value>::new();
    if let surrealdb::types::Value::Object(fields) = &row {
        for (_, field, _) in TAGGED_REFERENCES
            .iter()
            .filter(|(name, _, _)| *name == table.name)
        {
            if let Some(value) = fields.get(*field) {
                tagged_values.insert(*field, normalize_typed_record_links(value));
            }
        }
    }
    let mut row = row.into_json_value();
    if let Some((id, kind)) = natural_id {
        let fields = row.as_object_mut().ok_or(MirrorError::Source)?;
        fields.insert("id".to_owned(), Value::String(id));
        if MIXED_KEY_TABLES.contains(&table.name) {
            fields.insert("id_kind".to_owned(), Value::String(kind.to_owned()));
        }
    }
    if !tagged_values.is_empty() {
        let fields = row.as_object_mut().ok_or(MirrorError::Source)?;
        for (name, value) in tagged_values {
            fields.insert(name.to_owned(), value);
        }
    }
    normalize(table, row)
}

fn normalize_typed_record_links(value: &surrealdb::types::Value) -> Value {
    use surrealdb::types::{RecordIdKey, Value as SurrealValue};
    match value {
        SurrealValue::RecordId(record) => {
            let key = match &record.key {
                RecordIdKey::Uuid(id) => serde_json::json!({"Uuid": id.to_string()}),
                RecordIdKey::String(id) => serde_json::json!({"String": id}),
                RecordIdKey::Number(id) => serde_json::json!({"Number": id}),
                _ => Value::Null,
            };
            serde_json::json!({"tb": record.table.to_string(), "id": key})
        }
        SurrealValue::Array(values) => {
            Value::Array(values.iter().map(normalize_typed_record_links).collect())
        }
        SurrealValue::Object(fields) => Value::Object(
            fields
                .iter()
                .map(|(name, value)| (name.clone(), normalize_typed_record_links(value)))
                .collect(),
        ),
        other => other.clone().into_json_value(),
    }
}

fn string_record_key(table: &Table, value: &surrealdb::types::Value) -> Option<String> {
    let surrealdb::types::Value::RecordId(record_id) = value else {
        return None;
    };
    if record_id.table.as_str() != table.name {
        return None;
    }
    match &record_id.key {
        surrealdb::types::RecordIdKey::String(key) => Some(key.clone()),
        _ => None,
    }
}

fn timestamp(value: &Value) -> Result<Value, MirrorError> {
    let text = value.as_str().ok_or(MirrorError::SourceTimestamp)?;
    let parsed = DateTime::parse_from_rfc3339(text).map_err(|_| MirrorError::SourceTimestamp)?;
    Ok(Value::String(
        parsed
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Micros, true),
    ))
}

/// One row of either store in the comparable form both upsert and parity use.
fn normalize(table: &'static Table, row: Value) -> Result<(MirrorKey, Value), MirrorError> {
    let Value::Object(mut fields) = row else {
        return Err(MirrorError::Source);
    };
    fields.retain(|name, _| table.columns.contains(&name.as_str()));
    let mixed_id_kind = fields
        .get("id_kind")
        .and_then(Value::as_str)
        .map(str::to_owned);
    for name in table.columns {
        let value = fields.entry(*name).or_insert(Value::Null);
        if value.is_null() {
            continue;
        }
        if *name == "id" && STRING_KEY_TABLES.contains(&table.name) {
            *value = Value::String(string_key_in(value).ok_or(MirrorError::Source)?);
        } else if *name == "id" && MIXED_KEY_TABLES.contains(&table.name) {
            match mixed_id_kind.as_deref() {
                Some("string") => {
                    *value = Value::String(string_key_in(value).ok_or(MirrorError::Source)?);
                }
                Some("uuid") => {
                    *value = Value::String(uuid_in(value).ok_or(MirrorError::Source)?.to_string());
                }
                _ => return Err(MirrorError::Source),
            }
        } else if *name == "id" || table.references.iter().any(|(column, _)| column == name) {
            *value = Value::String(uuid_in(value).ok_or(MirrorError::Source)?.to_string());
        } else if table.timestamps.contains(name) {
            *value = timestamp(value)?;
        }
    }
    let id = fields.get("id").ok_or(MirrorError::Source)?;
    let key = if STRING_KEY_TABLES.contains(&table.name)
        || (MIXED_KEY_TABLES.contains(&table.name) && mixed_id_kind.as_deref() == Some("string"))
    {
        MirrorKey::RecordString {
            table: table.name,
            id: id.as_str().ok_or(MirrorError::Source)?.to_owned(),
        }
    } else {
        MirrorKey::Record {
            table: table.name,
            id: uuid_in(id).ok_or(MirrorError::Source)?,
        }
    };
    Ok((key, Value::Object(fields)))
}

async fn source(
    surreal: &SurrealDb,
    table: &'static Table,
) -> Result<BTreeMap<MirrorKey, Value>, MirrorError> {
    let rows = super::select_table(
        surreal,
        table.name,
        format!("SELECT * FROM `{}`", table.name),
    )
    .await?;
    rows.into_iter()
        .map(|row| normalize_surreal(table, row))
        .collect()
}

async fn source_one(
    surreal: &SurrealDb,
    table: &'static Table,
    id: Uuid,
) -> Result<Option<Value>, MirrorError> {
    let row: Option<surrealdb::types::Value> = surreal
        .query("SELECT * FROM ONLY $id")
        .bind(("id", crate::surreal::record_id(table.name, id)))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    row.map(|row| normalize_surreal(table, row).map(|(_, value)| value))
        .transpose()
}

async fn source_one_string(
    surreal: &SurrealDb,
    table: &'static Table,
    id: &str,
) -> Result<Option<Value>, MirrorError> {
    let row: Option<surrealdb::types::Value> = surreal
        .query("SELECT * FROM ONLY $id")
        .bind((
            "id",
            surrealdb::types::RecordId::new(table.name, id.to_owned()),
        ))
        .await
        .and_then(surrealdb::IndexedResults::check)
        .map_err(|_| MirrorError::Source)?
        .take(0)
        .map_err(|_| MirrorError::Source)?;
    row.map(|row| normalize_surreal(table, row).map(|(_, value)| value))
        .transpose()
}

async fn target(
    pg: &PostgresDb,
    table: &'static Table,
) -> Result<BTreeMap<MirrorKey, Value>, MirrorError> {
    // `table.name` is one of the static names above, never input.
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT to_jsonb(t) AS row FROM \"{}\" AS t",
        table.name
    )))
    .fetch_all(&pg.0)
    .await?;
    rows.iter()
        .map(|row| {
            let value: Value = row.try_get("row")?;
            normalize(table, value)
        })
        .collect()
}

fn upsert_sql(table: &Table) -> String {
    let rest = &table.columns[1..];
    let updates = rest
        .iter()
        .map(|column| format!("\"{column}\"=EXCLUDED.\"{column}\""))
        .collect::<Vec<_>>()
        .join(",");
    let current = rest
        .iter()
        .map(|column| format!("\"{}\".\"{column}\"", table.name))
        .collect::<Vec<_>>()
        .join(",");
    let excluded = rest
        .iter()
        .map(|column| format!("EXCLUDED.\"{column}\""))
        .collect::<Vec<_>>()
        .join(",");
    let name = table.name;
    let conflict = if MIXED_KEY_TABLES.contains(&table.name) {
        "(\"id_kind\", \"id\")"
    } else {
        "(\"id\")"
    };
    format!(
        "INSERT INTO \"{name}\" SELECT * FROM jsonb_populate_record(NULL::\"{name}\", $1::jsonb) \
         ON CONFLICT {conflict} DO UPDATE SET {updates} WHERE ROW({current}) IS DISTINCT FROM ROW({excluded})"
    )
}

/// The parent row a record link names, as the mirror that owns it keys it.
fn parent(table: &str, id: Uuid) -> Result<MirrorKey, MirrorError> {
    Ok(match table {
        "project" => MirrorKey::Project(id),
        "person" => MirrorKey::Person(id),
        "entity" => MirrorKey::Entity(id),
        "notation" => MirrorKey::Notation(id),
        "communication" => MirrorKey::Communication(id),
        _ => return Err(MirrorError::Source),
    })
}

/// Mirror each parent PostgreSQL does not yet hold, so the row's foreign keys
/// resolve.
async fn ensure_parents(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    table: &'static Table,
    row: &Value,
) -> Result<(), MirrorError> {
    // The legacy source tables have no reliable complete ownership graph;
    // keep their UUIDs losslessly and let `unresolved_references` report
    // orphans instead of synthesizing authority rows during import.
    if table.group == "source_domain_records" {
        return Ok(());
    }
    for (column, parent_table) in table.references {
        let Some(id) = row.get(*column).and_then(uuid_in) else {
            continue;
        };
        // `parent_table` is a static name from `TABLES`, never input.
        let held: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT EXISTS (SELECT 1 FROM \"{parent_table}\" WHERE id = $1)"
        )))
        .bind(id)
        .fetch_one(&pg.0)
        .await?;
        if !held {
            Box::pin(super::mirror(surreal, pg, &parent(parent_table, id)?)).await?;
        }
    }
    Ok(())
}

/// Make PostgreSQL's copy of one row match SurrealDB's.
///
/// # Errors
///
/// SurrealDB could not be read, or PostgreSQL refused the write.
pub async fn mirror(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    table_name: &str,
    id: Uuid,
) -> Result<Mirrored, MirrorError> {
    let table = table(table_name)?;
    let Some(row) = source_one(surreal, table, id).await? else {
        // `table.name` is one of the static names above, never input.
        let deleted = if MIXED_KEY_TABLES.contains(&table.name) {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM \"{}\" WHERE id_kind = 'uuid' AND id = $1",
                table.name
            )))
            .bind(id.to_string())
            .execute(&pg.0)
            .await?
        } else {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DELETE FROM \"{}\" WHERE id = $1",
                table.name
            )))
            .bind(id)
            .execute(&pg.0)
            .await?
        };
        return Ok(changed(deleted.rows_affected(), Mirrored::Deleted));
    };
    ensure_parents(surreal, pg, table, &row).await?;
    let written = sqlx::query(sqlx::AssertSqlSafe(upsert_sql(table)))
        .bind(row)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

/// Mirror a row whose SurrealDB record key is a domain-provided string.
pub async fn mirror_string(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    table_name: &str,
    id: &str,
) -> Result<Mirrored, MirrorError> {
    let table = table(table_name)?;
    if !STRING_KEY_TABLES.contains(&table.name) && !MIXED_KEY_TABLES.contains(&table.name) {
        return Err(MirrorError::Source);
    }
    let Some(row) = source_one_string(surreal, table, id).await? else {
        let statement = if MIXED_KEY_TABLES.contains(&table.name) {
            format!(
                "DELETE FROM \"{}\" WHERE id_kind = 'string' AND id = $1",
                table.name
            )
        } else {
            format!("DELETE FROM \"{}\" WHERE id = $1", table.name)
        };
        let deleted = sqlx::query(sqlx::AssertSqlSafe(statement))
            .bind(id)
            .execute(&pg.0)
            .await?;
        return Ok(changed(deleted.rows_affected(), Mirrored::Deleted));
    };
    ensure_parents(surreal, pg, table, &row).await?;
    let written = sqlx::query(sqlx::AssertSqlSafe(upsert_sql(table)))
        .bind(row)
        .execute(&pg.0)
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

fn tables(group: &str) -> impl Iterator<Item = &'static Table> + '_ {
    TABLES.iter().filter(move |table| table.group == group)
}

fn comparable(rows: BTreeMap<MirrorKey, Value>) -> BTreeMap<MirrorKey, Columns> {
    rows.into_iter()
        .map(|(key, row)| (key, Columns::Record(row)))
        .collect()
}

/// Compare every row of one group's tables.
///
/// # Errors
///
/// Either store could not be read.
pub async fn parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    group: &str,
) -> Result<ParityReport, MirrorError> {
    let mut reports = Vec::new();
    for table in tables(group) {
        reports.push(super::compare(
            table.name,
            &comparable(source(surreal, table).await?),
            &comparable(target(pg, table).await?),
        ));
    }
    Ok(ParityReport { tables: reports })
}

/// Re-mirror every divergent row of one group, then compare again. Rows
/// SurrealDB no longer has go first, so a replacement that reuses a unique
/// key lands after the row it replaces is gone.
///
/// # Errors
///
/// Either store could not be read for a comparison.
pub async fn reconcile(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    group: &str,
) -> Result<ParityReport, MirrorError> {
    let before = parity(surreal, pg, group).await?;
    for removing in [true, false] {
        for report in &before.tables {
            let held = source(surreal, table(report.table)?).await?;
            for key in &report.divergent {
                let (table, id, string_id) = match key {
                    MirrorKey::Record { table, id } => (*table, Some(*id), None),
                    MirrorKey::RecordString { table, id } => (*table, None, Some(id.as_str())),
                    _ => continue,
                };
                if held.contains_key(key) == removing {
                    continue;
                }
                let result = if let Some(id) = id {
                    mirror(surreal, pg, table, id).await
                } else {
                    mirror_string(surreal, pg, table, string_id.expect("string key")).await
                };
                if let Err(error) = result {
                    tracing::warn!(table, error = %error, "PostgreSQL reconcile could not mirror the row");
                }
            }
        }
    }
    parity(surreal, pg, group).await
}

#[cfg(test)]
#[path = "records_tests.rs"]
mod tests;
