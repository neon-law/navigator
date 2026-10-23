-- SurrealDB remains the write authority; these tables are an opt-in mirror.
CREATE TABLE email_receipt (
    id UUID PRIMARY KEY,
    receiving_mailbox TEXT NOT NULL,
    deployment TEXT NOT NULL,
    firm_id UUID REFERENCES firm(id),
    provider_account TEXT,
    sender_authenticated BOOLEAN NOT NULL DEFAULT false,
    raw_digest TEXT NOT NULL,
    source_message_id TEXT,
    archive_key TEXT NOT NULL,
    letter_id UUID NOT NULL,
    processing_state TEXT NOT NULL CHECK (processing_state IN ('pending', 'archived')),
    delivery_state TEXT NOT NULL CHECK (delivery_state = 'not_attempted'),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX email_receipt_identity
    ON email_receipt (receiving_mailbox, deployment, raw_digest);
CREATE INDEX email_receipt_firm ON email_receipt (firm_id, inserted_at DESC);

CREATE TABLE email_delivery (
    id UUID PRIMARY KEY,
    receipt_id UUID NOT NULL UNIQUE REFERENCES email_receipt(id) ON DELETE CASCADE,
    channel_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('not_attempted', 'sending', 'unknown', 'confirmed', 'failed')),
    current_correlation_id TEXT,
    slack_timestamp TEXT,
    last_error TEXT,
    reconciled_by TEXT,
    reconciliation TEXT,
    reconciled_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE email_delivery_attempt (
    id UUID PRIMARY KEY,
    delivery_id UUID NOT NULL REFERENCES email_delivery(id) ON DELETE CASCADE,
    correlation_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('sending', 'unknown', 'confirmed', 'failed')),
    slack_timestamp TEXT,
    error TEXT,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX email_delivery_attempt_delivery ON email_delivery_attempt (delivery_id, inserted_at);
