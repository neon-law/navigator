-- Versioned Template metadata. Bodies remain in their content-addressed Asset.
CREATE TABLE template (
    id UUID PRIMARY KEY,
    code TEXT NOT NULL,
    title TEXT NOT NULL,
    respondent_type TEXT NOT NULL,
    project_id UUID REFERENCES project(id),
    is_current BOOLEAN NOT NULL,
    asset_id UUID REFERENCES asset(id),
    form_code TEXT,
    kind TEXT,
    source_commit_sha TEXT,
    body_sha256 TEXT CHECK (body_sha256 IS NULL OR body_sha256 ~ '^[0-9a-f]{64}$'),
    current_key TEXT UNIQUE,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX template_code ON template (code);
