-- A Firm assumption: the stated premise the CEO digest checks actuals
-- against and a Kit resolves at run time (ENG-1264). `key` is a dotted path
-- from the repository's public `assumptions/schema.yaml` catalog, which
-- carries keys, types, and null defaults — never a Firm's values. The
-- values are Firm data, versioned and only ever added to: an assumption
-- change is a new version, so the history is the table
-- (docs/glossary/firm-assumption.md).
CREATE TABLE firm_assumption (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value JSONB NOT NULL,
    version INTEGER NOT NULL CHECK (version > 0),
    set_by UUID NOT NULL REFERENCES person(id),
    set_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    reason TEXT,
    UNIQUE (firm_id, key, version)
);
CREATE INDEX firm_assumption_current ON firm_assumption (firm_id, key, version DESC);

-- History: an assumption change is a new version, never an edit or a removal.
CREATE FUNCTION reject_firm_assumption_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Firm Assumption rows are versioned history; record a new version instead';
END;
$$;
CREATE TRIGGER firm_assumption_immutable
    BEFORE UPDATE OR DELETE ON firm_assumption
    FOR EACH ROW EXECUTE FUNCTION reject_firm_assumption_mutation();
CREATE TRIGGER firm_assumption_no_truncate
    BEFORE TRUNCATE ON firm_assumption
    FOR EACH STATEMENT EXECUTE FUNCTION reject_firm_assumption_mutation();
