-- A Legal Plan's fee schedule: what the plan pays for one covered benefit.
-- Rows are dated by effective_on and only ever added to; resolution takes
-- the latest row effective on or before the date asked, the attorney_rate
-- rule. Percentages never appear here — supplemental amounts are integer
-- cents, never floats. `matter_type` is a closed vocabulary shared with
-- project.plan_matter_type and the firm_assumption keys (ENG-1264);
-- `state` is a seeded jurisdiction code ('ny', 'nv', 'ca', 'wa'), the same
-- shape onboarding_jurisdiction stores.
CREATE TABLE plan_fee_schedule (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    legal_plan_id UUID NOT NULL REFERENCES legal_plan(id),
    matter_type TEXT NOT NULL CHECK (matter_type IN ('document_review', 'letter', 'uncontested_divorce', 'court_appearance')),
    state TEXT NOT NULL,
    covered_benefit TEXT NOT NULL,
    plan_fee_cents INTEGER NOT NULL CHECK (plan_fee_cents >= 0),
    supplemental_hourly_cents INTEGER CHECK (supplemental_hourly_cents IS NULL OR supplemental_hourly_cents >= 0),
    supplemental_flat_cents INTEGER CHECK (supplemental_flat_cents IS NULL OR supplemental_flat_cents >= 0),
    effective_on DATE NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (firm_id, legal_plan_id, matter_type, state, effective_on)
);
CREATE INDEX plan_fee_schedule_lookup ON plan_fee_schedule (legal_plan_id, matter_type, state, effective_on DESC);

-- A schedule row must belong to a Legal Plan of the same Firm; joining the
-- plan's firm_id proves it the way a Contingency Term tier proves its stage.
CREATE FUNCTION enforce_plan_fee_schedule_firm() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM legal_plan p
        WHERE p.id = NEW.legal_plan_id AND p.firm_id = NEW.firm_id
    ) THEN
        RAISE EXCEPTION 'A Plan Fee Schedule row must name a Legal Plan of the same Firm';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER plan_fee_schedule_firm
    BEFORE INSERT ON plan_fee_schedule
    FOR EACH ROW EXECUTE FUNCTION enforce_plan_fee_schedule_firm();

-- History: a change is a new effective_on, never an edit or a removal.
CREATE FUNCTION reject_plan_fee_schedule_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Plan Fee Schedule rows are dated history; record a new effective_on instead';
END;
$$;
CREATE TRIGGER plan_fee_schedule_immutable
    BEFORE UPDATE OR DELETE ON plan_fee_schedule
    FOR EACH ROW EXECUTE FUNCTION reject_plan_fee_schedule_mutation();
CREATE TRIGGER plan_fee_schedule_no_truncate
    BEFORE TRUNCATE ON plan_fee_schedule
    FOR EACH STATEMENT EXECUTE FUNCTION reject_plan_fee_schedule_mutation();
