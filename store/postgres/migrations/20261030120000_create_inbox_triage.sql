-- SurrealDB remains the write authority; the Inbox triage row is an opt-in mirror.
CREATE TABLE inbox_triage (
    id UUID PRIMARY KEY,
    message_id UUID NOT NULL REFERENCES message(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    suggested_project_id UUID REFERENCES project(id),
    project_confidence DOUBLE PRECISION,
    privilege TEXT NOT NULL CHECK (privilege IN ('privileged', 'not_privileged', 'uncertain')),
    privilege_confidence DOUBLE PRECISION,
    rationale TEXT,
    model_id TEXT,
    prompt_version TEXT NOT NULL,
    raw_output TEXT,
    disposition TEXT NOT NULL
        CHECK (disposition IN ('suggested', 'confirmed', 'overridden', 'needs_human')),
    labeled_example BOOLEAN NOT NULL DEFAULT FALSE,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT inbox_triage_one_per_message UNIQUE (message_id)
);
CREATE INDEX inbox_triage_firm ON inbox_triage (firm_id);
