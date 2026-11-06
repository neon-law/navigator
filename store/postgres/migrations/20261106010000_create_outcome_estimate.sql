-- A lawyer's Outcome Estimate for a matter: exactly three outcomes, each with
-- a probability in basis points and a gross recovery in integer cents. Rows
-- are only ever added to, like stage_event; the newest row is the current
-- estimate (docs/glossary/outcome-estimate.md). Firm work product: only a
-- firm-side participant reads or writes it.
CREATE TABLE outcome_estimate (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES project(id),
    author_person_id UUID NOT NULL REFERENCES person(id),
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    expected_resolution_on DATE NOT NULL,
    note TEXT CHECK (note IS NULL OR length(btrim(note)) BETWEEN 1 AND 2000),
    win_at_trial_probability_bps INTEGER NOT NULL CHECK (win_at_trial_probability_bps BETWEEN 0 AND 10000),
    win_at_trial_gross_recovery_cents BIGINT NOT NULL CHECK (win_at_trial_gross_recovery_cents >= 0),
    settle_probability_bps INTEGER NOT NULL CHECK (settle_probability_bps BETWEEN 0 AND 10000),
    settle_gross_recovery_cents BIGINT NOT NULL CHECK (settle_gross_recovery_cents >= 0),
    lose_probability_bps INTEGER NOT NULL CHECK (lose_probability_bps BETWEEN 0 AND 10000),
    lose_gross_recovery_cents BIGINT NOT NULL DEFAULT 0 CHECK (lose_gross_recovery_cents = 0),
    CHECK (win_at_trial_probability_bps + settle_probability_bps + lose_probability_bps = 10000)
);
CREATE INDEX outcome_estimate_project_recorded ON outcome_estimate (project_id, recorded_at, id);

CREATE FUNCTION reject_outcome_estimate_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Outcome Estimates are history; record a new estimate instead';
END;
$$;
CREATE TRIGGER outcome_estimate_immutable
    BEFORE UPDATE OR DELETE ON outcome_estimate
    FOR EACH ROW EXECUTE FUNCTION reject_outcome_estimate_mutation();
