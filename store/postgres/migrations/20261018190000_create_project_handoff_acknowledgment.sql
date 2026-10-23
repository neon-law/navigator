-- The recipient's acknowledgment of one handoff, bound to the exact manifest
-- digest they received. Born in PostgreSQL; immutable once written.
CREATE TABLE project_handoff_acknowledgment (
    handoff_id UUID PRIMARY KEY REFERENCES project_handoff(id),
    manifest_sha256 TEXT NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    acknowledged_by_person_id UUID NOT NULL,
    acknowledged_at TIMESTAMPTZ NOT NULL
);

CREATE TRIGGER project_handoff_acknowledgment_immutable
    BEFORE UPDATE OR DELETE ON project_handoff_acknowledgment
    FOR EACH ROW EXECUTE FUNCTION reject_handoff_mutation();
