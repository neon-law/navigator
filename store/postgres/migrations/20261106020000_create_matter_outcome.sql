-- A Matter Outcome: what actually happened when a matter resolved, and the
-- fee and costs the firm received (docs/glossary/matter-outcome.md). A matter
-- has one: the first row is the only one with no supersedes_id, and a
-- correction is a new row superseding the current one, never an edit. The
-- chain is the audit trail; its newest link is the outcome. Calibration
-- compares these actual figures against the matter's Outcome Estimates.
-- iolta_allocation_id names the IOLTA Allocation the recovery passed
-- through; allocations are mirrored from SurrealDB under text keys, so it
-- carries no foreign key.
CREATE TABLE matter_outcome (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES project(id),
    result TEXT NOT NULL CHECK (result IN ('won', 'settled', 'lost', 'dismissed')),
    resolved_on DATE NOT NULL,
    gross_recovery_cents BIGINT NOT NULL CHECK (gross_recovery_cents >= 0),
    fee_received_cents BIGINT NOT NULL CHECK (fee_received_cents >= 0),
    costs_recovered_cents BIGINT NOT NULL CHECK (costs_recovered_cents >= 0),
    iolta_allocation_id TEXT CHECK (iolta_allocation_id IS NULL OR length(btrim(iolta_allocation_id)) BETWEEN 1 AND 200),
    author_person_id UUID NOT NULL REFERENCES person(id),
    supersedes_id UUID UNIQUE REFERENCES matter_outcome(id),
    correction_reason TEXT CHECK (correction_reason IS NULL OR length(btrim(correction_reason)) BETWEEN 1 AND 500),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    CHECK ((supersedes_id IS NULL) = (correction_reason IS NULL))
);
-- One original per matter; UNIQUE (supersedes_id) keeps each correction
-- chain linear, so a matter holds exactly one current outcome.
CREATE UNIQUE INDEX matter_outcome_one_per_matter ON matter_outcome (project_id) WHERE supersedes_id IS NULL;

-- A correction must supersede a row of the same matter.
CREATE FUNCTION enforce_matter_outcome_chain() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.supersedes_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM matter_outcome WHERE id = NEW.supersedes_id AND project_id = NEW.project_id
    ) THEN
        RAISE EXCEPTION 'A Matter Outcome correction must supersede an outcome of the same matter';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER matter_outcome_chain
    BEFORE INSERT ON matter_outcome
    FOR EACH ROW EXECUTE FUNCTION enforce_matter_outcome_chain();

CREATE FUNCTION reject_matter_outcome_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'A Matter Outcome is never edited; record a superseding correction instead';
END;
$$;
CREATE TRIGGER matter_outcome_immutable
    BEFORE UPDATE OR DELETE ON matter_outcome
    FOR EACH ROW EXECUTE FUNCTION reject_matter_outcome_mutation();
