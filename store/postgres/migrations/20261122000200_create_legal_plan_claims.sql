CREATE TABLE legal_plan_claim (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    legal_plan_id UUID NOT NULL REFERENCES legal_plan(id),
    plan_case_number TEXT NOT NULL CHECK (length(btrim(plan_case_number)) > 0),
    state JSONB NOT NULL,
    UNIQUE (id, legal_plan_id)
);
CREATE INDEX legal_plan_claim_project ON legal_plan_claim (project_id, id);
CREATE INDEX legal_plan_claim_plan ON legal_plan_claim (legal_plan_id, id);

CREATE TABLE legal_plan_claim_event (
    id UUID PRIMARY KEY,
    claim_id UUID NOT NULL,
    legal_plan_id UUID NOT NULL,
    actor_id UUID NOT NULL REFERENCES person(id),
    occurred_on DATE NOT NULL,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp(),
    event JSONB NOT NULL,
    reference_kind TEXT CHECK (reference_kind IN ('claim', 'payment')),
    reference TEXT,
    CHECK ((reference_kind IS NULL) = (reference IS NULL)),
    FOREIGN KEY (claim_id, legal_plan_id) REFERENCES legal_plan_claim (id, legal_plan_id),
    UNIQUE (legal_plan_id, reference_kind, reference)
);
CREATE INDEX legal_plan_claim_event_claim ON legal_plan_claim_event (claim_id, recorded_at, id);

CREATE FUNCTION protect_legal_plan_claim_evidence() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Legal Plan Claim evidence is append-only';
END;
$$;
CREATE TRIGGER legal_plan_claim_evidence_immutable BEFORE UPDATE OR DELETE ON legal_plan_claim_event
    FOR EACH ROW EXECUTE FUNCTION protect_legal_plan_claim_evidence();
