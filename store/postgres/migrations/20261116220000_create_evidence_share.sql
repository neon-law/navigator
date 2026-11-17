CREATE TABLE evidence_share (
    id uuid PRIMARY KEY,
    project_id uuid NOT NULL,
    asset_id uuid NOT NULL,
    sha256 text NOT NULL,
    recipient_email text NOT NULL,
    created_by uuid NOT NULL,
    terms text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL DEFAULT now() + interval '14 days',
    revoked_at timestamptz,
    accepted_by uuid,
    accepted_at timestamptz,
    opens bigint NOT NULL DEFAULT 0,
    downloads bigint NOT NULL DEFAULT 0,
    CHECK ((accepted_by IS NULL) = (accepted_at IS NULL))
);

CREATE INDEX evidence_share_document ON evidence_share (project_id, asset_id, created_at DESC);

CREATE TABLE evidence_share_usage (
    day date PRIMARY KEY,
    opens bigint NOT NULL,
    downloads bigint NOT NULL
);
