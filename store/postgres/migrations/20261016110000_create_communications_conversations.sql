-- SurrealDB remains the write authority; these tables are an opt-in mirror.
CREATE TABLE communication (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    channel TEXT NOT NULL,
    direction TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound', 'internal')),
    author_person_id UUID REFERENCES person(id),
    counterparty TEXT,
    subject TEXT,
    body TEXT NOT NULL,
    source_ref TEXT,
    asset_id UUID REFERENCES asset(id),
    occurred_at TIMESTAMPTZ NOT NULL,
    call JSONB,
    agreements JSONB NOT NULL DEFAULT '[]'::jsonb,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX communication_channel_source_ref
    ON communication (channel, source_ref) WHERE source_ref IS NOT NULL;
CREATE INDEX communication_project_occurred ON communication (project_id, occurred_at);

CREATE TABLE email_conversation (
    id UUID PRIMARY KEY,
    token TEXT NOT NULL UNIQUE,
    external_email TEXT NOT NULL,
    external_name TEXT,
    person_id UUID REFERENCES person(id),
    subject TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('open', 'awaiting_lawyer', 'awaiting_client', 'closed')),
    notation_id UUID REFERENCES notation(id),
    firm_id UUID REFERENCES firm(id),
    project_id UUID REFERENCES project(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX email_conversation_external_email ON email_conversation (external_email);
CREATE INDEX email_conversation_scope ON email_conversation (firm_id, project_id);

CREATE TABLE email_conversation_message (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES email_conversation(id) ON DELETE CASCADE,
    direction TEXT NOT NULL CHECK (direction IN ('from_external', 'to_lawyer', 'from_lawyer', 'to_external', 'system')),
    from_addr TEXT NOT NULL,
    to_addr TEXT NOT NULL,
    subject TEXT NOT NULL,
    body_text TEXT NOT NULL,
    raw_storage_key TEXT,
    provider_message_id TEXT,
    in_reply_to TEXT,
    command_payload TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX email_conversation_message_conversation ON email_conversation_message (conversation_id);
