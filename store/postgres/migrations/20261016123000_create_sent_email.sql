-- Outbound audit is append-only. PostgreSQL mirrors never overwrite history.
CREATE TABLE sent_email (
    id UUID PRIMARY KEY,
    recipient TEXT NOT NULL,
    subject TEXT NOT NULL,
    sender TEXT NOT NULL,
    project_id UUID REFERENCES project(id),
    template_slug TEXT,
    body TEXT NOT NULL,
    outcome TEXT NOT NULL,
    sg_message_id TEXT,
    sent_at TIMESTAMPTZ NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX sent_email_project_sent_at ON sent_email (project_id, sent_at DESC, id DESC);
