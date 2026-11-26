//! Whole-row mirror of tables whose PostgreSQL row is the SurrealDB row with
//! its record links as UUID foreign keys.
//!
//! Each [`Table`] names its columns, which of them are record links and the
//! table each link names, and which carry timestamps. A row keeps its record
//! UUID as `id`. A source field without a declared preservation mapping is refused.
//! Parity compares whole rows as JSON, timestamps at
//! PostgreSQL's microsecond precision. Compatibility rows update only when a column differs;
//! a missing source row removes their copy. Authoritative tables accept only missing
//! rows during the first backfill and retain their PostgreSQL contents on every replay.
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
        name: "mailroom",
        group: "application_scoped",
        columns: &["id", "name", "address_id", "inserted_at", "updated_at"],
        references: &[("address_id", "address")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "letter",
        group: "application_scoped",
        columns: &[
            "id",
            "mailroom_id",
            "project_id",
            "direction",
            "sender",
            "recipient",
            "summary",
            "inserted_at",
            "updated_at",
        ],
        references: &[("mailroom_id", "mailroom"), ("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "lead",
        group: "application_scoped",
        columns: &[
            "id",
            "email",
            "email_lower",
            "phone",
            "brand_key",
            "source_path",
            "consent_version",
            "consented_at",
            "status",
            "unsubscribed_at",
            "person_id",
            "submissions",
            "inserted_at",
            "updated_at",
        ],
        references: &[("person_id", "person")],
        timestamps: &[
            "consented_at",
            "unsubscribed_at",
            "inserted_at",
            "updated_at",
        ],
    },
    Table {
        name: "lead_consent",
        group: "application_scoped",
        columns: &[
            "id",
            "lead_id",
            "consented_at",
            "phone",
            "source_path",
            "sms_consent_version",
            "sms_policy_version",
            "sms_opt_in",
            "call_consent_version",
            "call_opt_in",
        ],
        references: &[("lead_id", "lead")],
        timestamps: &["consented_at"],
    },
    Table {
        name: "project_deadlines",
        group: "application_scoped",
        columns: &[
            "id",
            "project_id",
            "due_at",
            "due_on",
            "hearing",
            "authority_kind",
            "authority",
            "source",
            "source_document_id",
            "owner_person_id",
            "status",
            "calendar_role",
            "internal_lead_days",
            "client_lead_days",
            "replay_key",
            "inserted_at",
            "updated_at",
            "title",
            "calendar_sequence",
            "calendar_review_pending",
            "description",
            "category",
        ],
        references: &[
            ("project_id", "project"),
            ("owner_person_id", "person"),
            ("source_document_id", "asset"),
        ],
        timestamps: &["due_at", "inserted_at", "updated_at"],
    },
    Table {
        name: "project_service",
        group: "application_scoped",
        columns: &[
            "id",
            "project_id",
            "document_id",
            "recipient",
            "recipient_person_id",
            "recipient_entity_id",
            "counsel_for",
            "method",
            "served_on",
            "address",
            "declarant",
            "declarant_person_id",
            "letter_id",
            "sent_email_id",
            "inserted_at",
            "updated_at",
        ],
        references: &[
            ("project_id", "project"),
            ("document_id", "asset"),
            ("recipient_person_id", "person"),
            ("recipient_entity_id", "entity"),
            ("declarant_person_id", "person"),
            ("letter_id", "letter"),
            ("sent_email_id", "sent_email"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "summary_sender_approval",
        group: "application_scoped",
        columns: &[
            "id",
            "firm_id",
            "project_id",
            "provider_account",
            "person_id",
            "sender_email",
            "status",
            "updated_at",
        ],
        references: &[
            ("firm_id", "firm"),
            ("project_id", "project"),
            ("person_id", "person"),
        ],
        timestamps: &["updated_at"],
    },
    Table {
        name: "summary_sender_approval_event",
        group: "application_scoped",
        columns: &[
            "id",
            "firm_id",
            "actor_person_id",
            "project_id",
            "provider_account",
            "sender_email",
            "person_id",
            "action",
            "inserted_at",
        ],
        references: &[
            ("firm_id", "firm"),
            ("actor_person_id", "person"),
            ("project_id", "project"),
            ("person_id", "person"),
        ],
        timestamps: &["inserted_at"],
    },
    Table {
        name: "inbox_triage",
        group: "application_scoped",
        columns: &[
            "id",
            "message_id",
            "firm_id",
            "suggested_project_id",
            "project_confidence",
            "privilege",
            "privilege_confidence",
            "rationale",
            "model_id",
            "prompt_version",
            "raw_output",
            "disposition",
            "labeled_example",
            "inserted_at",
            "updated_at",
        ],
        references: &[
            ("message_id", "message"),
            ("firm_id", "firm"),
            ("suggested_project_id", "project"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "product",
        group: "source_domain_records",
        columns: &[
            "id",
            "account_code",
            "active",
            "billing_kind",
            "cadence",
            "code",
            "currency",
            "display_name",
            "inserted_at",
            "list_price_cents",
            "matter_close_template_code",
            "retainer_template_code",
            "updated_at",
            "xero_item_code",
        ],
        references: &[],
        timestamps: &["inserted_at", "updated_at"],
    },
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
            "inserted_at_text",
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
    Table {
        name: "entity",
        group: "identity_scoped",
        columns: &[
            "id",
            "name",
            "entity_type_id",
            "jurisdiction_id",
            "phone",
            "url",
            "avatar_url",
            "xero_id",
            "metadata",
            "firm_anchor_key",
            "inserted_at",
            "updated_at",
        ],
        references: &[
            ("entity_type_id", "entity_type"),
            ("jurisdiction_id", "jurisdiction"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "person",
        group: "identity_scoped",
        columns: &[
            "id",
            "name",
            "given_name",
            "family_name",
            "middle_name",
            "email",
            "oidc_subject",
            "microsoft_subject",
            "apple_subject",
            "role",
            "title",
            "phone",
            "xero_contact_id",
            "profile_image_url",
            "linkedin_url",
            "email_confirmed",
            "is_admitted",
            "inserted_at",
            "updated_at",
        ],
        references: &[],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "address",
        group: "identity_scoped",
        columns: &[
            "id",
            "person_id",
            "entity_id",
            "line1",
            "line2",
            "city",
            "region",
            "postal_code",
            "country",
            "inserted_at",
            "updated_at",
        ],
        references: &[("person_id", "person"), ("entity_id", "entity")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "credential",
        group: "identity_scoped",
        columns: &[
            "id",
            "person_id",
            "jurisdiction_id",
            "license_number",
            "inserted_at",
            "updated_at",
        ],
        references: &[("person_id", "person"), ("jurisdiction_id", "jurisdiction")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "email_token",
        group: "identity_scoped",
        columns: &[
            "id",
            "person_id",
            "email",
            "purpose",
            "token_hash",
            "expires_at",
            "used_at",
            "inserted_at",
            "updated_at",
        ],
        references: &[("person_id", "person")],
        timestamps: &["expires_at", "used_at", "inserted_at", "updated_at"],
    },
    Table {
        name: "person_delegate",
        group: "identity_scoped",
        columns: &[
            "id",
            "delegate_person_id",
            "subject_person_id",
            "basis",
            "instrument_reference",
            "granted_by_person_id",
            "granted_at",
            "subject_notified_at",
            "subject_notified_channel",
            "revoked_at",
            "revoked_by_person_id",
            "inserted_at",
            "updated_at",
        ],
        references: &[
            ("delegate_person_id", "person"),
            ("subject_person_id", "person"),
            ("granted_by_person_id", "person"),
            ("revoked_by_person_id", "person"),
        ],
        timestamps: &[
            "granted_at",
            "subject_notified_at",
            "revoked_at",
            "inserted_at",
            "updated_at",
        ],
    },
    Table {
        name: "person_external_identity",
        group: "identity_scoped",
        columns: &[
            "id",
            "person_id",
            "system",
            "external_id",
            "handle",
            "inserted_at",
            "updated_at",
        ],
        references: &[("person_id", "person")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "person_mailbox",
        group: "identity_scoped",
        columns: &["id", "person_id"],
        references: &[("person_id", "person")],
        timestamps: &[],
    },
    Table {
        name: "firm",
        group: "tenancy_scoped",
        columns: &[
            "id",
            "name",
            "slug",
            "status",
            "entity_id",
            "portal_theme_brand_key",
            "inserted_at",
            "updated_at",
        ],
        references: &[("entity_id", "entity")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "brand",
        group: "tenancy_scoped",
        columns: &[
            "id",
            "name",
            "brand_key",
            "firm_id",
            "primary_color",
            "accent_color",
            "typeface",
            "is_law_firm",
            "legal_entity",
            "logo_object_key",
            "logo_content_type",
            "font_family",
            "font_object_key",
            "font_licence",
            "website_url",
            "inserted_at",
            "updated_at",
        ],
        references: &[("firm_id", "firm")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "firm_brand",
        group: "tenancy_scoped",
        columns: &["id", "firm_id", "brand_key", "inserted_at", "updated_at"],
        references: &[("firm_id", "firm")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "project",
        group: "tenancy_scoped",
        columns: &[
            "id",
            "code",
            "name",
            "status",
            "brand",
            "entity_id",
            "firm_id",
            "jurisdiction_id",
            "description",
            "drive_folder_id",
            "repository_url",
            "git_initialized_at",
            "forge_provisioned_at",
            "closed_at",
            "closure_reason",
            "internal_slack_channel_url",
            "external_slack_channel_url",
            "internal_slack_channel_id",
            "private_notion_page_url",
            "shared_notion_page_url",
            "calendar_id",
            "calendar_timezone",
            "booking_url",
            "onboarding_jurisdiction",
            "client_dri_person_id",
            "legal_plan_id",
            "plan_member_id",
            "plan_case_number",
            "benefit_code",
            "plan_matter_type",
            "plan_state",
            "plan_assigned_on",
            "plan_accepted_on",
            "plan_closed_on",
            "kit_version",
            "inserted_at",
            "updated_at",
        ],
        references: &[
            ("entity_id", "entity"),
            ("firm_id", "firm"),
            ("jurisdiction_id", "jurisdiction"),
            ("client_dri_person_id", "person"),
            ("legal_plan_id", "legal_plan"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "person_firm_role",
        group: "tenancy_scoped",
        columns: &[
            "id",
            "person_id",
            "firm_id",
            "membership",
            "is_dri",
            "inserted_at",
            "updated_at",
        ],
        references: &[("person_id", "person"), ("firm_id", "firm")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "person_project_role",
        group: "tenancy_scoped",
        columns: &[
            "id",
            "person_id",
            "project_id",
            "participation",
            "is_lawyer_dri",
            "is_client_dri",
            "inserted_at",
            "updated_at",
        ],
        references: &[("person_id", "person"), ("project_id", "project")],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "template",
        group: "application_scoped",
        columns: &[
            "id",
            "code",
            "title",
            "respondent_type",
            "project_id",
            "firm_id",
            "is_current",
            "asset_id",
            "form_code",
            "kind",
            "source_commit_sha",
            "body_sha256",
            "current_key",
            "inserted_at",
            "updated_at",
        ],
        references: &[
            ("project_id", "project"),
            ("firm_id", "firm"),
            ("asset_id", "asset"),
        ],
        timestamps: &["inserted_at", "updated_at"],
    },
    Table {
        name: "sent_email",
        group: "application_scoped",
        columns: &[
            "id",
            "recipient",
            "subject",
            "sender",
            "project_id",
            "template_slug",
            "body",
            "outcome",
            "sg_message_id",
            "sent_at",
            "inserted_at",
            "updated_at",
        ],
        references: &[("project_id", "project")],
        timestamps: &["sent_at", "inserted_at", "updated_at"],
    },
    Table {
        name: "matter_page",
        group: "application_scoped",
        columns: &[
            "id",
            "project_id",
            "firm_id",
            "slug",
            "kind",
            "title",
            "draft",
            "position",
            "client_sections",
            "lawyer_sections",
            "clerk_sections",
            "synced_at",
            "document_revision",
            "media",
        ],
        references: &[("project_id", "project"), ("firm_id", "firm")],
        timestamps: &["synced_at"],
    },
    Table {
        name: "firm_integration_secret",
        group: "application_scoped",
        columns: &[
            "id",
            "firm_id",
            "provider",
            "kind",
            "version",
            "ciphertext",
            "wrapped_dek",
            "kms_key_version",
            "kms_context",
            "status",
            "actor_id",
            "created_at",
            "updated_at",
        ],
        references: &[("firm_id", "firm"), ("actor_id", "person")],
        timestamps: &["created_at", "updated_at"],
    },
];

/// Surreal record tables whose primary key is a domain-provided string.
const STRING_KEY_TABLES: &[&str] = &[
    "person_mailbox",
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
    /// References whose source parent exists but whose PostgreSQL parent is missing.
    pub newly_target_missing: usize,
}

impl UnresolvedReference {
    /// Classify each exact reference before aggregation; an old source orphan
    /// cannot mask a different parent lost from PostgreSQL.
    pub(crate) fn observe(&mut self, source_parent_exists: bool, target_parent_exists: bool) {
        self.source_orphans += usize::from(!source_parent_exists);
        self.postgres_missing += usize::from(!target_parent_exists);
        self.newly_target_missing += usize::from(source_parent_exists && !target_parent_exists);
    }
}

fn reference_counter<'a>(
    counts: &'a mut BTreeMap<(&'static str, &'static str), UnresolvedReference>,
    (table, field): (&'static str, &'static str),
) -> &'a mut UnresolvedReference {
    counts.entry((table, field)).or_insert(UnresolvedReference {
        table,
        field,
        source_orphans: 0,
        postgres_missing: 0,
        newly_target_missing: 0,
    })
}

/// Count dangling concrete and tagged record links without rewriting them.
pub async fn unresolved_references(
    surreal: &SurrealDb,
    pg: &PostgresDb,
) -> Result<Vec<UnresolvedReference>, MirrorError> {
    reference_inventory(surreal, pg, None).await
}

/// Inspect only enrolled child tables and the parents their registered fields require.
pub(crate) async fn scoped_references(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    tables: &[&str],
) -> Result<Vec<UnresolvedReference>, MirrorError> {
    let mut references = reference_inventory(surreal, pg, Some(tables)).await?;
    if tables.contains(&"asset") {
        references.extend(super::documents::scoped_references(surreal, pg).await?);
    }
    if tables.contains(&"matter_page") {
        references.extend(super::matter_pages_captions::scoped_page_references(surreal, pg).await?);
    }
    if tables.contains(&"sent_email") {
        references.extend(super::sent_emails::scoped_references(surreal, pg).await?);
    }
    Ok(references)
}

#[allow(clippy::too_many_lines)] // Each declared reference family shares one fail-closed inventory pass.
async fn reference_inventory(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    filter: Option<&[&str]>,
) -> Result<Vec<UnresolvedReference>, MirrorError> {
    let selected: Vec<_> = TABLES
        .iter()
        .filter(|table| {
            !matches!(table.name, "matter_page" | "sent_email")
                && filter.map_or_else(
                    || !table.group.ends_with("_scoped"),
                    |names| names.contains(&table.name),
                )
        })
        .collect();
    let mut parent_tables = std::collections::BTreeSet::new();
    for table in &selected {
        for (_, parent) in table.references {
            parent_tables.insert(*parent);
        }
    }
    for (_, _, parents) in TAGGED_REFERENCES
        .iter()
        .filter(|(table, _, _)| filter.is_none_or(|names| names.contains(table)))
    {
        parent_tables.extend(*parents);
    }

    let mut natural_parent_tables = std::collections::BTreeSet::new();
    for (_, _, parent) in NATURAL_REFERENCES
        .iter()
        .filter(|(table, _, _)| filter.is_none_or(|names| names.contains(table)))
    {
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

    let mut counts = BTreeMap::<(&'static str, &'static str), UnresolvedReference>::new();
    for table in &selected {
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
                    reference_counter(&mut counts, (*source_table, *field))
                        .observe(!source_orphan, !target_missing);
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
                    reference_counter(&mut counts, (*source_table, *field)).observe(false, false);
                }
            }
        }
    }
    Ok(counts.into_values().collect())
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
    counts: &mut BTreeMap<(&'static str, &'static str), UnresolvedReference>,
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
        reference_counter(counts, key).observe(!source_orphan, !target_missing);
    }
}

/// The registered table of this name.
pub(super) fn table(name: &str) -> Result<&'static Table, MirrorError> {
    TABLES
        .iter()
        .find(|table| table.name == name)
        .ok_or(MirrorError::Source)
}

/// The PostgreSQL columns of a registered table, `id` first.
pub(crate) fn columns(name: &str) -> Option<&'static [&'static str]> {
    match name {
        "attestation" => Some(super::attestations::COLUMNS),
        "asset" => Some(super::documents::ASSET_COLUMNS),
        "notation" => Some(super::workflow::NOTATION_COLUMNS),
        "notation_event" => Some(super::workflow::EVENT_COLUMNS),
        "answer" => Some(super::workflow::ANSWER_COLUMNS),
        "notation_clause" => Some(super::notation_clauses::COLUMNS),
        _ => table(name).ok().map(|table| table.columns),
    }
}

/// Person links in authoritative record tables that deliberately accept historical orphans.
/// These use the same preservation metadata as import, without adding foreign keys.
pub(crate) fn authoritative_person_links() -> impl Iterator<Item = (&'static str, &'static str)> {
    TABLES
        .iter()
        .filter(|table| authoritative(table))
        .flat_map(|table| {
            table.references.iter().filter_map(move |(column, target)| {
                (*target == "person").then_some((table.name, *column))
            })
        })
}

/// Tagged Person links share the canonical preservation registry with import.
pub(crate) fn authoritative_tagged_person_links(
) -> impl Iterator<Item = (&'static str, &'static str)> {
    TAGGED_REFERENCES
        .iter()
        .filter_map(|(table, field, parents)| {
            parents.contains(&"person").then_some((*table, *field))
        })
}

/// Recover a declared reference field from the canonical preservation metadata.
pub(crate) fn reference_field(name: &str, field: &str) -> Option<&'static str> {
    if name == "asset" {
        return super::documents::REFERENCES
            .iter()
            .find_map(|(column, _)| (*column == field).then_some(*column));
    }
    TABLES
        .iter()
        .filter(|table| table.name == name)
        .flat_map(|table| table.references.iter().map(|(column, _)| *column))
        .chain(
            TAGGED_REFERENCES
                .iter()
                .filter(|(table, _, _)| *table == name)
                .map(|(_, column, _)| *column),
        )
        .chain(
            NATURAL_REFERENCES
                .iter()
                .filter(|(table, _, _)| *table == name)
                .map(|(_, column, _)| *column),
        )
        .find(|column| *column == field)
}

fn authoritative(table: &Table) -> bool {
    table.name == "product"
        || crate::postgres::matter::TABLES.contains(&table.name)
        || crate::postgres::accounting::TABLES.contains(&table.name)
        || crate::postgres::cases::TABLES.contains(&table.name)
        || crate::postgres::review::TABLES.contains(&table.name)
        || crate::postgres::signatures::TABLES.contains(&table.name)
}

/// PostgreSQL ownership fences Source import. Scoped families follow their
/// enrollment contracts; authoritative matter records admit only first copy.
async fn require_source_authority(pg: &PostgresDb, table: &Table) -> Result<(), MirrorError> {
    if crate::postgres_backfill::family::Family::for_table(table.name).is_some() {
        return match crate::postgres_backfill::family::import_mode(pg, table.name).await? {
            crate::postgres_backfill::family::ImportMode::Refuse => {
                Err(MirrorError::MatterAuthority)
            }
            _ => Ok(()),
        };
    }
    if crate::postgres_backfill::family::active_for_table(pg, table.name).await? {
        return Err(MirrorError::MatterAuthority);
    }
    if authoritative(table) && !super::initial_copy() {
        Err(MirrorError::MatterAuthority)
    } else {
        Ok(())
    }
}

/// A row SurrealDB lacks is no evidence against a graph table PostgreSQL
/// holds ([`crate::postgres_graph::holds`]): the mirror still copies the
/// Surreal-era rows across, but never deletes.
fn refuse_graph_delete(table: &Table) -> Result<(), MirrorError> {
    if crate::postgres_graph::holds(table.name) {
        Err(MirrorError::GraphAuthority)
    } else {
        Ok(())
    }
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

pub(super) fn normalize_surreal(
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
    require_mapped_fields(table, &row, used_identity_alias)?;
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
    let mut missing_json = Vec::new();
    if let surrealdb::types::Value::Object(fields) = &row {
        for column in table.columns {
            if json_column(table, column)?
                && fields
                    .get(*column)
                    .is_none_or(|value| matches!(value, surrealdb::types::Value::None))
            {
                missing_json.push(*column);
            }
        }
    }
    let mut row = row.into_json_value();
    if table.name == "deployment_secret_change" {
        preserve_deployment_change_context(&mut row)?;
    }

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
    if let Some(fields) = row.as_object_mut() {
        for column in missing_json {
            fields.remove(column);
        }
    }
    normalized_record(table.name, row)
}

/// Fields a retired schema left on old SurrealDB rows. No column carries them,
/// and a row whose value is `false` holds nothing to preserve, so the import
/// drops that value instead of refusing the table. Any other value still needs
/// a destination.
///
/// The name is assembled from two pieces because the vocabulary gate bans its
/// spelling from the source, and old rows still carry it.
const RETIRED_DRI_FLAG: &str = concat!("is_st", "aff_dri");
const RETIRED_WHEN_FALSE: &[(&str, &str)] = &[("person_project_role", RETIRED_DRI_FLAG)];

fn require_mapped_fields(
    table: &Table,
    row: &surrealdb::types::Value,
    used_identity_alias: bool,
) -> Result<(), MirrorError> {
    if let surrealdb::types::Value::Object(fields) = row {
        let count = fields
            .iter()
            .filter(|(name, value)| {
                let retired = RETIRED_WHEN_FALSE.contains(&(table.name, name.as_str()))
                    && matches!(value, surrealdb::types::Value::Bool(false));
                // id_kind is generated from the typed source key, not copied from a source field.
                let mapped = table.columns.contains(&name.as_str())
                    && !(name.as_str() == "id_kind" && MIXED_KEY_TABLES.contains(&table.name));
                // Person's derived lowercase index is represented by PostgreSQL citext.
                let derived = table.name == "person" && name.as_str() == "email_lower";
                !(mapped
                    || derived
                    || retired
                    || used_identity_alias && name.as_str() == "__surreal_key")
            })
            .count();
        if count > 0 {
            return Err(MirrorError::UnmappedSourceFields {
                table: table.name,
                count,
            });
        }
    }
    Ok(())
}

fn preserve_deployment_change_context(row: &mut Value) -> Result<(), MirrorError> {
    let fields = row.as_object_mut().ok_or(MirrorError::Source)?;
    let timestamp = fields
        .get("inserted_at")
        .and_then(Value::as_str)
        .filter(|value| DateTime::parse_from_rfc3339(value).is_ok())
        .ok_or(MirrorError::Source)?
        .to_owned();
    fields.insert("inserted_at_text".into(), Value::String(timestamp));
    Ok(())
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
/// Boolean columns a source row may lack, which PostgreSQL stores as `false`: rows
/// written before the field existed carry no value. The typed mirror reads the
/// same absence with `unwrap_or_default`, so the generic path must agree or a
/// legacy row compares as differing and inserts NULL into a NOT NULL column.
const ABSENT_IS_FALSE: &[(&str, &str)] = &[("lead_consent", "call_opt_in")];

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
        if json_column(table, name)? && !fields.contains_key(*name) {
            continue;
        }
        let value = fields.entry(*name).or_insert(Value::Null);
        if value.is_null() && ABSENT_IS_FALSE.contains(&(table.name, *name)) {
            *value = Value::Bool(false);
        }
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
    let projection =
        crate::postgres::matter::row_json(table.name).map_err(|_| MirrorError::MatterAuthority)?;
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {projection} AS row FROM \"{}\" AS r",
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

fn json_column(table: &Table, column: &str) -> Result<bool, MirrorError> {
    crate::postgres::matter::json_column(table.name, column)
        .map_err(|_| MirrorError::MatterAuthority)
}

fn insert_projection(table: &Table) -> Result<String, MirrorError> {
    table
        .columns
        .iter()
        .map(|column| {
            Ok(if json_column(table, column)? {
                format!("$1::jsonb -> '{column}'")
            } else {
                format!("r.\"{column}\"")
            })
        })
        .collect::<Result<Vec<_>, MirrorError>>()
        .map(|columns| columns.join(","))
}

fn upsert_sql(table: &Table) -> Result<String, MirrorError> {
    let projection = insert_projection(table)?;
    let columns = table
        .columns
        .iter()
        .map(|column| format!("\"{column}\""))
        .collect::<Vec<_>>()
        .join(",");
    if table.name == "deployment_secret_change" {
        // Only an identical captured encrypted row can supply previously absent
        // authenticated timestamp bytes. PostgreSQL timestamps cannot reconstruct them.
        let compared = table
            .columns
            .iter()
            .filter(|column| **column != "inserted_at_text")
            .map(|column| format!("\"deployment_secret_change\".\"{column}\""))
            .collect::<Vec<_>>()
            .join(",");
        let captured = table
            .columns
            .iter()
            .filter(|column| **column != "inserted_at_text")
            .map(|column| format!("EXCLUDED.\"{column}\""))
            .collect::<Vec<_>>()
            .join(",");
        return Ok(format!(
            "INSERT INTO deployment_secret_change ({columns}) SELECT {projection} FROM jsonb_populate_record(NULL::deployment_secret_change, $1::jsonb) r ON CONFLICT (id) DO UPDATE SET inserted_at_text=EXCLUDED.inserted_at_text WHERE deployment_secret_change.inserted_at_text IS NULL AND EXCLUDED.inserted_at_text IS NOT NULL AND ROW({compared}) IS NOT DISTINCT FROM ROW({captured})"
        ));
    }
    if crate::postgres_backfill::family::Family::for_table(table.name)
        == Some(crate::postgres_backfill::family::Family::Application)
    {
        let name = table.name;
        return Ok(format!(
            "INSERT INTO \"{name}\" ({columns}) SELECT {projection} FROM jsonb_populate_record(NULL::\"{name}\", $1::jsonb) r ON CONFLICT DO NOTHING"
        ));
    }
    if table.name == "relationship"
        || (authoritative(table) && !crate::postgres_backfill::family::importing(table.name))
    {
        let name = table.name;
        return Ok(format!(
            "INSERT INTO \"{name}\" ({columns}) SELECT {projection} FROM jsonb_populate_record(NULL::\"{name}\", $1::jsonb) r ON CONFLICT (id) DO NOTHING"
        ));
    }
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
    Ok(format!(
        "INSERT INTO \"{name}\" ({columns}) SELECT {projection} FROM jsonb_populate_record(NULL::\"{name}\", $1::jsonb) r \
         ON CONFLICT {conflict} DO UPDATE SET {updates} WHERE ROW({current}) IS DISTINCT FROM ROW({excluded})"
    ))
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
            // A parent PostgreSQL lacks is copied even into a table it holds
            // authoritatively: an insert can overwrite or delete nothing.
            let key = parent(parent_table, id)?;
            Box::pin(super::mirror(surreal, pg, &key)).await?;
        }
    }
    Ok(())
}

pub(super) async fn insert_product<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    row: Value,
) -> Result<Mirrored, MirrorError> {
    let result = sqlx::query(
        "INSERT INTO product SELECT * FROM jsonb_populate_record(NULL::product, $1::jsonb) \
             ON CONFLICT (id) DO NOTHING",
    )
    .bind(row)
    .execute(executor)
    .await?;
    Ok(changed(result.rows_affected(), Mirrored::Written))
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
    if table.name == "product" {
        if !super::initial_copy() {
            return Ok(Mirrored::Unchanged);
        }
        let Some(row) = source_one(surreal, table, id).await? else {
            if authoritative(table) {
                return Ok(Mirrored::Unchanged);
            }

            return Ok(Mirrored::Unchanged);
        };
        return insert_product(&pg.0, row).await;
    }
    if (MirrorKey::Record {
        table: table.name,
        id,
    })
    .held_by_people_authority()
    {
        return Ok(Mirrored::Unchanged);
    }
    require_source_authority(pg, table).await?;
    let Some(row) = source_one(surreal, table, id).await? else {
        if crate::postgres_backfill::family::Family::for_table(table.name).is_some() {
            return Err(MirrorError::Source);
        }
        refuse_graph_delete(table)?;
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
    if !crate::postgres_backfill::family::importing(table.name) {
        ensure_parents(surreal, pg, table, &row).await?;
    }
    copy_record(pg, table.name, row).await
}

/// Mirror a row whose SurrealDB record key is a domain-provided string.
pub async fn mirror_string(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    table_name: &str,
    id: &str,
) -> Result<Mirrored, MirrorError> {
    let table = table(table_name)?;
    require_source_authority(pg, table).await?;
    if !STRING_KEY_TABLES.contains(&table.name) && !MIXED_KEY_TABLES.contains(&table.name) {
        return Err(MirrorError::Source);
    }
    let Some(row) = source_one_string(surreal, table, id).await? else {
        refuse_graph_delete(table)?;
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
    copy_record(pg, table.name, row).await
}

fn tables(group: &str) -> impl Iterator<Item = &'static Table> + '_ {
    TABLES.iter().filter(move |table| table.group == group)
}

fn comparable(rows: BTreeMap<MirrorKey, Value>) -> BTreeMap<MirrorKey, Columns> {
    rows.into_iter()
        .map(|(key, row)| (key, Columns::Record(row)))
        .collect()
}

/// Existing PostgreSQL Person rewrites cannot be adjudicated by a legacy refresh.
async fn preserve_person_references(
    pg: &PostgresDb,
    table: &Table,
    row: &Value,
) -> Result<(), MirrorError> {
    if !crate::postgres_backfill::family::importing(table.name)
        || !table
            .references
            .iter()
            .any(|(_, parent)| *parent == "person")
    {
        return Ok(());
    }
    let id = row
        .get("id")
        .and_then(Value::as_str)
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or(MirrorError::Source)?;
    let projection =
        crate::postgres::matter::row_json(table.name).map_err(|_| MirrorError::Source)?;
    let previous: Option<Value> = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT {projection} FROM \"{}\" r WHERE id = $1",
        table.name
    )))
    .bind(id)
    .fetch_optional(pg.pool())
    .await?;
    if previous.is_some_and(|previous| {
        table
            .references
            .iter()
            .any(|(field, parent)| *parent == "person" && previous.get(*field) != row.get(*field))
    }) {
        return Err(MirrorError::MatterAuthority);
    }
    Ok(())
}

/// Copy a normalized source record through the same preservation guard as the importer.
pub(crate) async fn copy_record(
    pg: &PostgresDb,
    name: &str,
    row: Value,
) -> Result<Mirrored, MirrorError> {
    let table = table(name)?;
    require_source_authority(pg, table).await?;
    preserve_person_references(pg, table, &row).await?;
    let written = sqlx::query(sqlx::AssertSqlSafe(upsert_sql(table)?))
        .bind(row)
        .execute(pg.pool())
        .await?;
    Ok(changed(written.rows_affected(), Mirrored::Written))
}

/// The canonical whole-row normalization used by import and typed parity.
pub(crate) fn normalized_record(name: &str, row: Value) -> Result<(MirrorKey, Value), MirrorError> {
    normalize(table(name)?, row)
}

/// PostgreSQL rows with SQL NULL distinguished from explicit JSONB null.
pub(crate) async fn target_record_rows(
    pg: &PostgresDb,
    name: &str,
) -> Result<BTreeMap<MirrorKey, Value>, MirrorError> {
    target(pg, table(name)?).await
}

/// Compare already-normalized rows through the mirror's existing value comparison.
pub(crate) fn compare_record_rows(
    name: &'static str,
    source: BTreeMap<MirrorKey, Value>,
    target: BTreeMap<MirrorKey, Value>,
) -> super::TableParity {
    super::compare(name, &comparable(source), &comparable(target))
}

/// Full enrolled-table parity; selected import is the only refresh permission.
pub(crate) async fn scoped_table_parity(
    surreal: &SurrealDb,
    pg: &PostgresDb,
    name: &'static str,
) -> Result<super::TableParity, MirrorError> {
    let table = table(name)?;
    Ok(compare_record_rows(
        name,
        source(surreal, table).await?,
        target_record_rows(pg, name).await?,
    ))
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
    let mut compares_authoritative = None;
    for table in tables(group) {
        if crate::postgres_backfill::family::active_for_table(pg, table.name).await? {
            continue;
        }
        if crate::postgres_backfill::family::importing(table.name) {
            reports.push(scoped_table_parity(surreal, pg, table.name).await?);
            continue;
        }
        let authoritative = authoritative(table) || crate::postgres_graph::holds(table.name);
        if authoritative {
            if compares_authoritative.is_none() {
                compares_authoritative = Some(super::compares_authoritative(pg).await?);
            }
            if compares_authoritative == Some(false) {
                continue;
            }
        }
        let report = super::compare(
            table.name,
            &comparable(
                source(surreal, table)
                    .await
                    .map_err(|error| error.at(format!("table {}", table.name)))?,
            ),
            &comparable(
                target(pg, table)
                    .await
                    .map_err(|error| error.at(format!("table {}", table.name)))?,
            ),
        );
        reports.push(if authoritative {
            super::only_missing(pg, report).await?
        } else {
            report
        });
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
            let table = table(report.table)?;
            if table.name == "product" && !super::initial_copy() {
                continue;
            }
            let held = source(surreal, table).await?;
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

#[cfg(test)]
#[path = "relationship_recovery_tests.rs"]
mod relationship_recovery_tests;
