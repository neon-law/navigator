CREATE TABLE firm_integration_secret (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    kind TEXT NOT NULL,
    version BIGINT NOT NULL CHECK (version > 0),
    ciphertext TEXT NOT NULL,
    wrapped_dek TEXT NOT NULL,
    kms_key_version TEXT NOT NULL,
    kms_context TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('active', 'revoked')),
    actor_id UUID NOT NULL REFERENCES person(id),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (firm_id, provider, kind, version)
);

CREATE INDEX firm_integration_secret_active
    ON firm_integration_secret (firm_id, provider, kind, version DESC)
    WHERE status = 'active';
