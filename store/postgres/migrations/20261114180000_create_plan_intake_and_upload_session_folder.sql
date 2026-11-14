-- `plan_intake` and `document_upload_session` are born in PostgreSQL: whenever
-- `NAVIGATOR_POSTGRES_HOST` is set the application reads and writes them here,
-- with no write mirror.

-- The matter folder an Upload Session files into at completion (LAW-241).
ALTER TABLE document_upload_session ADD COLUMN folder text;

-- One staged plan-assignment notice per inbound provider email, held for an
-- attorney's attested acceptance; `accepted` rows name the opened Project and
-- `declined` rows keep their reason for the audit trail. Staging is idempotent
-- per `source_message_id`. The Firm, People, Entity, Project, and inbox
-- references are plain UUIDs: those rows' authority is still selected per
-- deployment, so a staged notice never waits on their PostgreSQL copy.
-- `conflict_summary` holds one conflict finding per line.
CREATE TABLE plan_intake (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    conversation_id UUID NOT NULL,
    source_message_id UUID NOT NULL UNIQUE,
    provider TEXT NOT NULL,
    member_name TEXT,
    member_email TEXT,
    member_person_id UUID,
    member_entity_id UUID,
    plan_member_id TEXT,
    benefit_code TEXT,
    plan_number TEXT,
    plan_matter_type TEXT NOT NULL,
    plan_state TEXT,
    assigned_at TIMESTAMPTZ NOT NULL,
    legal_plan_id UUID,
    legal_plan_name TEXT,
    plan_portal_only BOOLEAN NOT NULL,
    fee_covered_benefit TEXT,
    fee_plan_fee_cents INTEGER,
    fee_supplemental_flat_cents INTEGER,
    fee_supplemental_hourly_cents INTEGER,
    fee_effective_on DATE,
    conflict_summary TEXT NOT NULL,
    conflict_blocking BOOLEAN NOT NULL,
    conflict_checked BOOLEAN NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('staged', 'accepted', 'declined')),
    project_id UUID,
    accepted_by UUID,
    decided_at TIMESTAMPTZ,
    decline_reason TEXT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX plan_intake_firm ON plan_intake (firm_id, assigned_at DESC);
