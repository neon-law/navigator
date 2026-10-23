-- Firm-scoped authorization state and its append-only audit trail.
CREATE UNIQUE INDEX project_id_firm_id ON project (id, firm_id);

CREATE TABLE summary_sender_approval (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    project_id UUID,
    provider_account TEXT NOT NULL,
    person_id UUID NOT NULL REFERENCES person(id),
    sender_email TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('approved', 'revoked')),
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id)
);

CREATE UNIQUE INDEX summary_sender_approval_identity
    ON summary_sender_approval (firm_id, project_id, provider_account, sender_email);
CREATE INDEX summary_sender_approval_firm
    ON summary_sender_approval (firm_id, updated_at DESC);

CREATE TABLE summary_sender_approval_event (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    project_id UUID,
    provider_account TEXT NOT NULL,
    sender_email TEXT NOT NULL,
    person_id UUID NOT NULL REFERENCES person(id),
    action TEXT NOT NULL CHECK (action IN ('approved', 'revoked')),
    inserted_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, firm_id) REFERENCES project (id, firm_id)
);
CREATE INDEX summary_sender_approval_event_firm
    ON summary_sender_approval_event (firm_id, inserted_at DESC);
