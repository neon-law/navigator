ALTER TABLE time_entry DROP CONSTRAINT time_entry_source_check;
ALTER TABLE time_entry ADD CONSTRAINT time_entry_source_check CHECK (source IN ('timer', 'manual', 'jujutsu'));

CREATE TABLE billing_annotation (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    attorney_id UUID NOT NULL REFERENCES person(id),
    change_id TEXT NOT NULL CHECK (change_id ~ '^[k-z]{32}$'),
    commit_id TEXT NOT NULL CHECK (commit_id ~ '^([0-9a-f]{40}|[0-9a-f]{64})$'),
    performed_on DATE NOT NULL,
    tenths INTEGER NOT NULL CHECK (tenths BETWEEN 1 AND 240),
    staged_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    reviewed_by UUID REFERENCES person(id),
    reviewed_at TIMESTAMPTZ,
    review_note TEXT CHECK (length(btrim(review_note)) BETWEEN 1 AND 2000),
    entry_id UUID UNIQUE REFERENCES time_entry(id),
    UNIQUE (project_id, change_id),
    FOREIGN KEY (project_id, attorney_id) REFERENCES person_project_role (project_id, person_id),
    CONSTRAINT billing_annotation_review_evidence CHECK (
        (reviewed_by IS NULL AND reviewed_at IS NULL AND review_note IS NULL AND entry_id IS NULL) OR (reviewed_by IS NOT NULL AND reviewed_at IS NOT NULL AND review_note IS NOT NULL)
    )
);
CREATE FUNCTION protect_billing_annotation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'UPDATE' AND OLD.reviewed_at IS NULL AND NEW.reviewed_at IS NOT NULL AND
       (to_jsonb(OLD) - ARRAY['reviewed_at','reviewed_by','review_note','entry_id']) =
       (to_jsonb(NEW) - ARRAY['reviewed_at','reviewed_by','review_note','entry_id']) THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'Billing annotations retain their source and review decision';
END;
$$;
CREATE TRIGGER billing_annotation_review BEFORE UPDATE OR DELETE ON billing_annotation
    FOR EACH ROW EXECUTE FUNCTION protect_billing_annotation();

ALTER TABLE matter_billing_batch ADD COLUMN trust_state TEXT CHECK (trust_state IS NULL OR (stripe_iolta AND trust_state ~ '^[A-Z]{2}$'));
