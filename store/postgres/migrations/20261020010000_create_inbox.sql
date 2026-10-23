-- SurrealDB remains the write authority; Inbox rows are opt-in mirrors.
CREATE TABLE inbox_conversation (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    brand_key TEXT,
    interaction_type TEXT NOT NULL DEFAULT 'message',
    provider_post_id TEXT,
    project_id UUID REFERENCES project(id),
    channel TEXT NOT NULL CHECK (channel IN ('email', 'sms', 'facebook', 'instagram', 'threads', 'youtube', 'tiktok')),
    external_address TEXT NOT NULL,
    external_name TEXT,
    subject TEXT,
    unread BOOLEAN NOT NULL DEFAULT TRUE,
    provider_account TEXT NOT NULL,
    provider_thread_ref TEXT,
    source_page TEXT,
    source_channel TEXT,
    matter_consent BOOLEAN NOT NULL DEFAULT FALSE,
    promotional_opt_in BOOLEAN NOT NULL DEFAULT FALSE,
    promotional_opt_out BOOLEAN NOT NULL DEFAULT FALSE,
    email_conversation_id UUID REFERENCES email_conversation(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX inbox_conversation_firm ON inbox_conversation (firm_id);
CREATE INDEX inbox_conversation_email ON inbox_conversation (email_conversation_id);

CREATE TABLE message (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES inbox_conversation(id),
    parent_message_id UUID REFERENCES message(id),
    visibility TEXT CHECK (visibility IN ('public', 'private')),
    direction TEXT NOT NULL CHECK (direction IN ('inbound', 'outbound', 'internal')),
    body TEXT NOT NULL,
    author_person_id UUID REFERENCES person(id),
    provider_message_id TEXT,
    provider_ref TEXT,
    delivery_state TEXT NOT NULL CHECK (delivery_state IN ('received', 'queued', 'submitted', 'delivered', 'unknown', 'failed', 'suppressed')),
    purpose TEXT NOT NULL DEFAULT 'matter' CHECK (purpose IN ('matter', 'promotional')),
    attachment_asset_id UUID REFERENCES asset(id),
    communication_id UUID REFERENCES communication(id),
    occurred_at TEXT NOT NULL,
    alert_state TEXT CHECK (alert_state IN ('pending', 'notified', 'unknown', 'failed', 'disabled')),
    alert_channel_id TEXT,
    alert_timestamp TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX message_conversation ON message (conversation_id);

CREATE TABLE inbox_ingress (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    channel TEXT NOT NULL,
    provider_account TEXT NOT NULL,
    provider_message_id TEXT NOT NULL,
    message_id UUID NOT NULL REFERENCES message(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT inbox_ingress_key UNIQUE (firm_id, channel, provider_account, provider_message_id)
);

CREATE TABLE inbox_assignment (
    id UUID PRIMARY KEY,
    conversation_id UUID NOT NULL REFERENCES inbox_conversation(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    from_project_id UUID REFERENCES project(id),
    to_project_id UUID NOT NULL REFERENCES project(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX inbox_assignment_conversation ON inbox_assignment (conversation_id);

CREATE TABLE inbox_delivery_attempt (
    id UUID PRIMARY KEY,
    message_id UUID NOT NULL REFERENCES message(id),
    state TEXT NOT NULL,
    provider_ref TEXT,
    detail TEXT,
    inserted_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX inbox_delivery_attempt_message ON inbox_delivery_attempt (message_id);

CREATE FUNCTION reject_inbox_audit_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Inbox audit rows are immutable';
END;
$$;
CREATE TRIGGER inbox_ingress_immutable
    BEFORE UPDATE OR DELETE ON inbox_ingress
    FOR EACH ROW EXECUTE FUNCTION reject_inbox_audit_mutation();
CREATE TRIGGER inbox_assignment_immutable
    BEFORE UPDATE OR DELETE ON inbox_assignment
    FOR EACH ROW EXECUTE FUNCTION reject_inbox_audit_mutation();
CREATE TRIGGER inbox_delivery_attempt_immutable
    BEFORE UPDATE OR DELETE ON inbox_delivery_attempt
    FOR EACH ROW EXECUTE FUNCTION reject_inbox_audit_mutation();
