-- Project-scoped Deadline records. The Project FK is the ownership boundary;
-- linked Documents and owners remain optional as in the Surreal source.
CREATE TABLE project_deadlines (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    due_at TIMESTAMPTZ NOT NULL,
    authority_kind TEXT NOT NULL CHECK (authority_kind IN ('statute', 'court_rule', 'court_order', 'contract', 'internal')),
    authority TEXT NOT NULL,
    source TEXT NOT NULL,
    source_document_id UUID REFERENCES asset(id),
    owner_person_id UUID REFERENCES person(id),
    status TEXT NOT NULL CHECK (status IN ('open', 'complete', 'cancelled')),
    calendar_role TEXT NOT NULL CHECK (calendar_role IN ('firm', 'opponent', 'hearing')),
    internal_lead_days BIGINT CHECK (internal_lead_days >= 0),
    client_lead_days BIGINT CHECK (client_lead_days >= 0),
    replay_key TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK (client_lead_days IS NULL OR internal_lead_days IS NOT NULL),
    CHECK (client_lead_days IS NULL OR internal_lead_days >= client_lead_days)
);
CREATE UNIQUE INDEX project_deadlines_replay_key
    ON project_deadlines (project_id, replay_key);
CREATE INDEX project_deadlines_due_at
    ON project_deadlines (project_id, status, due_at);
