-- A Brand's encrypted provider connection. The source key is the stable
-- Surreal record key; ciphertext and wrapped key stay byte-for-byte intact.
CREATE UNIQUE INDEX brand_key_firm_id ON brand (brand_key, firm_id);

CREATE TABLE social_account (
    id TEXT PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    brand_key TEXT NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok')),
    provider_account_id TEXT NOT NULL,
    display_name TEXT,
    scopes TEXT[] NOT NULL,
    expires_at TIMESTAMPTZ,
    ciphertext TEXT NOT NULL,
    wrapped_dek TEXT NOT NULL,
    kms_key_version TEXT NOT NULL,
    kms_context TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('connected', 'revoked')),
    connected_by_person_id UUID NOT NULL REFERENCES person(id),
    revoked_by_person_id UUID REFERENCES person(id),
    revoked_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (brand_key, firm_id) REFERENCES brand(brand_key, firm_id),
    UNIQUE (brand_key, provider, provider_account_id),
    CHECK ((status = 'connected' AND revoked_at IS NULL AND revoked_by_person_id IS NULL) OR (status = 'revoked' AND revoked_at IS NOT NULL AND revoked_by_person_id IS NOT NULL)),
    CHECK ((status = 'connected' AND ciphertext <> '' AND wrapped_dek <> '') OR (status = 'revoked' AND ciphertext = '' AND wrapped_dek = ''))
);

CREATE INDEX social_account_firm_status ON social_account (firm_id, status, provider);
