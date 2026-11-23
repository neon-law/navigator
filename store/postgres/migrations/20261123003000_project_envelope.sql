CREATE TABLE project_envelope (
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    account_id UUID NOT NULL,
    envelope_id UUID NOT NULL,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    observed_at TIMESTAMPTZ NOT NULL,
    evidence JSONB NOT NULL,
    PRIMARY KEY (firm_id, account_id, envelope_id)
);

CREATE INDEX project_envelope_project ON project_envelope (project_id, observed_at DESC);
