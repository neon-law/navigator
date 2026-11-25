-- Historical proposals predate immutable archive receipts.
ALTER TABLE deadline_proposal ADD COLUMN source_receipts JSONB;
ALTER TABLE deadline_proposal ADD CONSTRAINT deadline_proposal_source_receipts_check CHECK (source_receipts IS NULL OR (jsonb_typeof(source_receipts) = 'array' AND jsonb_array_length(source_receipts) = COALESCE(jsonb_array_length(draft->'evidence'),0)));

CREATE FUNCTION protect_deadline_proposal_revision() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'a saved court-date proposal revision is immutable';
END;
$$;
CREATE TRIGGER deadline_proposal_revision_immutable
    BEFORE UPDATE OF project_id, id, revision, draft, target_deadline_id, source_receipts ON deadline_proposal
    FOR EACH ROW WHEN ((OLD.project_id, OLD.id, OLD.revision, OLD.draft, OLD.target_deadline_id, OLD.source_receipts)
                       IS DISTINCT FROM (NEW.project_id, NEW.id, NEW.revision, NEW.draft, NEW.target_deadline_id, NEW.source_receipts))
    EXECUTE FUNCTION protect_deadline_proposal_revision();
