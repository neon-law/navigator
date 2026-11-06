-- A matter's Contingency Term: its percentage-of-recovery fee. Rows are dated
-- by effective_on and only ever added to; resolution takes the latest row
-- effective on or before the date asked (docs/glossary/contingency-term.md).
-- Percentages are basis points and amounts integer cents, never floats.
-- fee_agreement_notation_id names the signed fee-agreement Notation the
-- engagement letter requires; Notations are mirrored from SurrealDB, so the
-- store checks it and it carries no foreign key, like notation.template_id.
CREATE TABLE contingency_term (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES project(id),
    effective_on DATE NOT NULL,
    costs_deducted TEXT NOT NULL CHECK (costs_deducted IN ('before_fee', 'after_fee')),
    rate_cents_per_hour INTEGER CHECK (rate_cents_per_hour IS NULL OR rate_cents_per_hour >= 0),
    fee_agreement_notation_id UUID NOT NULL,
    author_person_id UUID NOT NULL REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX contingency_term_project_effective ON contingency_term (project_id, effective_on, inserted_at);

-- One tier per Case Stage: the fee a resolution at or after that stage pays.
-- `ordinal` is the authored order, which follows the Firm's stage order.
CREATE TABLE contingency_term_tier (
    contingency_term_id UUID NOT NULL REFERENCES contingency_term(id),
    ordinal SMALLINT NOT NULL CHECK (ordinal >= 0),
    stage_definition_id UUID NOT NULL REFERENCES stage_definition(id),
    fee_bps INTEGER NOT NULL CHECK (fee_bps BETWEEN 0 AND 10000),
    PRIMARY KEY (contingency_term_id, ordinal),
    UNIQUE (contingency_term_id, stage_definition_id)
);

-- A tier's stage must come from the matter Firm's own vocabulary.
CREATE FUNCTION enforce_contingency_term_tier_stage() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM contingency_term t
        JOIN project p ON p.id = t.project_id
        JOIN stage_definition d ON d.firm_id = p.firm_id
        WHERE t.id = NEW.contingency_term_id AND d.id = NEW.stage_definition_id
    ) THEN
        RAISE EXCEPTION 'A Contingency Term tier must name a stage from the matter Firm''s own vocabulary';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER contingency_term_tier_stage
    BEFORE INSERT ON contingency_term_tier
    FOR EACH ROW EXECUTE FUNCTION enforce_contingency_term_tier_stage();

-- Both tables are history: a change is a new effective_on, never an edit or
-- a removal.
CREATE FUNCTION reject_contingency_term_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Contingency Terms are dated history; record a new effective_on instead';
END;
$$;
CREATE TRIGGER contingency_term_immutable
    BEFORE UPDATE OR DELETE ON contingency_term
    FOR EACH ROW EXECUTE FUNCTION reject_contingency_term_mutation();
CREATE TRIGGER contingency_term_tier_immutable
    BEFORE UPDATE OR DELETE ON contingency_term_tier
    FOR EACH ROW EXECUTE FUNCTION reject_contingency_term_mutation();
