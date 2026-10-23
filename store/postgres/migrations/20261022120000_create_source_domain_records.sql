-- Full-row PostgreSQL destinations for persistent Surreal source records.

-- Direct record links retain UUIDs; polymorphic links and relation endpoints retain table tags as JSONB.

-- These link columns have no PostgreSQL FK so source orphans remain importable and are reported separately.

CREATE TABLE "authority_use" (
    "authority_id" UUID NOT NULL,
    "disposition" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "position" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "role" TEXT,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "cap_table_holding" (
    "holder_id" JSONB NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "project_id" UUID NOT NULL,
    "share_class" TEXT NOT NULL,
    "shares" BIGINT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "vesting_note" TEXT
);

CREATE TABLE "case" (
    "caption" TEXT NOT NULL,
    "docket_number" TEXT,
    "forum" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "judge" TEXT,
    "jurisdiction" TEXT,
    "posture" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "case_docket_entry" (
    "case_id" UUID NOT NULL,
    "document_asset_id" UUID,
    "entry_number" TEXT NOT NULL,
    "filed_or_served_on" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "integrity_checked_at" TIMESTAMPTZ,
    "integrity_matched" BOOLEAN,
    "integrity_source_sha256_hex" TEXT,
    "kind" TEXT NOT NULL,
    "notation_id" UUID,
    "party" TEXT,
    "scheduled_on" TIMESTAMPTZ,
    "stamp_case_number" TEXT,
    "stamp_entry_number" TEXT,
    "stamp_filed_on" TEXT,
    "supersedes" UUID,
    "title" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "citation" (
    "authority_use_id" UUID NOT NULL,
    "draft_pin" JSONB NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "quote" TEXT NOT NULL,
    "source_pin" JSONB NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "why" TEXT NOT NULL
);

CREATE TABLE "contract_review" (
    "asset_id" UUID,
    "findings" JSONB NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID NOT NULL,
    "playbook_id" UUID NOT NULL,
    "risk_summary" TEXT,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "deployment_secret_change" (
    "actor_id" TEXT NOT NULL,
    "ciphertext" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "key" TEXT NOT NULL,
    "kms_key_version" TEXT NOT NULL,
    "request_id" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "wrapped_key" TEXT NOT NULL
);

CREATE TABLE "disclosure" (
    "entity_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "kind" TEXT NOT NULL,
    "project_id" UUID,
    "summary" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "discovery_item" (
    "discovery_request_id" UUID NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "item_number" BIGINT NOT NULL,
    "objections" TEXT,
    "request_text" TEXT NOT NULL,
    "response_text" TEXT,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "discovery_request" (
    "case_id" UUID NOT NULL,
    "device" TEXT NOT NULL,
    "docket_entry_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "propounding_party" TEXT NOT NULL,
    "responding_party" TEXT NOT NULL,
    "responses_due_on" TEXT,
    "served_on" TEXT,
    "set_number" BIGINT NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "document_comment" (
    "anchor_end" BIGINT NOT NULL,
    "anchor_start" BIGINT NOT NULL,
    "body" TEXT NOT NULL,
    "communication_id" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "person_id" UUID NOT NULL,
    "quoted_text" TEXT NOT NULL,
    "resolved" BOOLEAN NOT NULL,
    "review_document_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "efiling_transactions" (
    "accepted_on" TEXT,
    "case_number" TEXT NOT NULL,
    "conformed_document_id" UUID,
    "court" TEXT NOT NULL,
    "efsp" TEXT NOT NULL,
    "filed_on" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "needs_entered_order" BOOLEAN NOT NULL,
    "note" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "status" TEXT NOT NULL,
    "submitted_document_id" UUID,
    "transaction_id" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "estate_asset" (
    "category" TEXT NOT NULL,
    "description" TEXT NOT NULL,
    "funding_state" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "note" TEXT,
    "passes_by" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "expunge_record" (
    "authorized_by_person_id" UUID NOT NULL,
    "category" TEXT NOT NULL,
    "head_after" TEXT,
    "head_before" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "note" TEXT,
    "path" TEXT NOT NULL,
    "project_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "expunge_request" (
    "asset_id" UUID NOT NULL,
    "expunge_record_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "note" TEXT,
    "project_id" UUID NOT NULL,
    "requested_by_person_id" UUID NOT NULL,
    "resolved_by_person_id" UUID,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "fiduciary_role" (
    "fiduciary_id" JSONB NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID,
    "project_id" UUID NOT NULL,
    "role" TEXT NOT NULL,
    "successor_order" BIGINT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "filing" (
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "kind" TEXT NOT NULL,
    "notation_id" UUID NOT NULL,
    "office" TEXT NOT NULL,
    "reference" TEXT,
    "submitted_at" TIMESTAMPTZ NOT NULL,
    "summary" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "finance_aggregate" (
    "computed_at" TIMESTAMPTZ NOT NULL,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "parameters" JSONB NOT NULL,
    "project_id" UUID NOT NULL,
    "report_json" TEXT NOT NULL,
    "run_id" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "firm_anchor" (
    "entity_id" JSONB NOT NULL,
    "id" TEXT NOT NULL,
    "id_kind" TEXT NOT NULL CHECK ("id_kind" IN ('string', 'uuid')),
    PRIMARY KEY ("id_kind", "id")
);

CREATE TABLE "iolta_account" (
    "balance_cents" BIGINT NOT NULL,
    "currency" TEXT NOT NULL,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "jurisdiction_id" UUID NOT NULL,
    "mirrored_at" TIMESTAMPTZ,
    "name" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "xero_account_code" TEXT,
    "xero_account_id" TEXT NOT NULL
);

CREATE TABLE "iolta_allocation" (
    "amount_cents" BIGINT NOT NULL,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "invoice_reference" TEXT NOT NULL,
    "occurred_at" TIMESTAMPTZ NOT NULL,
    "project_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "withdrawal_id" TEXT NOT NULL,
    "xero_invoice_id" TEXT NOT NULL
);

CREATE TABLE "iolta_withdrawal" (
    "currency" TEXT NOT NULL,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "jurisdiction_id" UUID NOT NULL,
    "occurred_at" TIMESTAMPTZ NOT NULL,
    "total_cents" BIGINT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "xero_transaction_id" TEXT NOT NULL
);

CREATE TABLE "notarization" (
    "asset_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notarized_at" TIMESTAMPTZ,
    "notary_person_id" UUID,
    "notation_id" UUID NOT NULL,
    "provider" TEXT NOT NULL,
    "provider_id" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "notation_document" (
    "current_version_id" UUID,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID NOT NULL,
    "original_asset_id" UUID NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "notation_document_comment" (
    "anchor" TEXT NOT NULL,
    "body" TEXT NOT NULL,
    "decision" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "person_id" UUID NOT NULL,
    "resolved" BOOLEAN NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "version_id" UUID NOT NULL
);

CREATE TABLE "notation_document_version" (
    "anchor_manifest_asset_id" UUID NOT NULL,
    "authored_by_person_id" UUID NOT NULL,
    "document_id" UUID NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "markdown_asset_id" UUID NOT NULL,
    "parent_version_id" UUID,
    "parser_version" TEXT NOT NULL,
    "schema_version" BIGINT NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "notation_meter_entry" (
    "currency" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID,
    "package_id" UUID,
    "project_id" UUID NOT NULL,
    "quantity" BIGINT NOT NULL,
    "rate_id" UUID NOT NULL,
    "state" TEXT NOT NULL,
    "unit_amount_cents" BIGINT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "relationship" (
    "confidence_pct" BIGINT NOT NULL,
    "detail" TEXT,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "kind" TEXT NOT NULL,
    "source_id" JSONB,
    "source_kind" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "in" JSONB NOT NULL,
    "out" JSONB NOT NULL
);

CREATE TABLE "review_document" (
    "body_html" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "kind" TEXT NOT NULL,
    "notation_id" UUID NOT NULL,
    "status" TEXT NOT NULL,
    "title" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "signature" (
    "completed_signer_roles" JSONB,
    "field" TEXT,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "notation_id" UUID NOT NULL,
    "provider" TEXT NOT NULL,
    "provider_id" TEXT NOT NULL,
    "signed_at" TIMESTAMPTZ,
    "signer_person_id" UUID,
    "state" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);

CREATE TABLE "social_oauth_attempt" (
    "actor_person_id" UUID NOT NULL,
    "brand_key" TEXT NOT NULL,
    "consumed_at" TIMESTAMPTZ,
    "expires_at" TIMESTAMPTZ NOT NULL,
    "firm_id" UUID NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "instagram_account_id" TEXT,
    "pkce_verifier" TEXT NOT NULL,
    "provider" TEXT NOT NULL,
    "provider_account_id" TEXT,
    "state_hash" TEXT NOT NULL
);

CREATE TABLE "verification" (
    "citation_id" UUID NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "revision_sha" TEXT NOT NULL,
    "status_citation" TEXT NOT NULL,
    "status_proposition" TEXT NOT NULL,
    "status_quote" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "verifier_person_id" UUID NOT NULL
);

CREATE TABLE "visitor_route_count" (
    "bucket_date" TIMESTAMPTZ NOT NULL,
    "country_code" TEXT NOT NULL,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "locale" TEXT NOT NULL,
    "route_pattern" TEXT NOT NULL,
    "source" TEXT NOT NULL,
    "status_class" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "visits" BIGINT NOT NULL
);

CREATE TABLE "xero_invoice" (
    "amount_cents" BIGINT NOT NULL,
    "amount_paid_cents" BIGINT NOT NULL,
    "currency" TEXT NOT NULL,
    "due_at" TIMESTAMPTZ,
    "id" TEXT NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "issued_at" TIMESTAMPTZ NOT NULL,
    "project_id" UUID NOT NULL,
    "reference" TEXT NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL,
    "xero_invoice_id" TEXT NOT NULL
);
