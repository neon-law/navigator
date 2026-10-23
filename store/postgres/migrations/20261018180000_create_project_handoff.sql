-- A legal-files-only handoff to one recipient: the immutable manifest of the
-- exact revisions, sizes, and checksums the recipient may receive. Born in
-- PostgreSQL; the bytes stay in Navigator storage.
CREATE TABLE project_handoff (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    project_id UUID NOT NULL,
    recipient_person_id UUID NOT NULL,
    lens TEXT NOT NULL CHECK (lens IN ('client', 'lawyer')),
    required BOOLEAN NOT NULL,
    manifest JSONB NOT NULL,
    manifest_sha256 TEXT NOT NULL CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    file_count INTEGER NOT NULL CHECK (file_count >= 0),
    total_bytes BIGINT NOT NULL CHECK (total_bytes >= 0),
    created_by_person_id UUID NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX project_handoff_project ON project_handoff (project_id, created_at);

CREATE FUNCTION reject_handoff_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'A handoff manifest is immutable';
END;
$$;
CREATE TRIGGER project_handoff_immutable
    BEFORE UPDATE OR DELETE ON project_handoff
    FOR EACH ROW EXECUTE FUNCTION reject_handoff_mutation();
