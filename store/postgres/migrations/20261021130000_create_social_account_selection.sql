-- A saved destination has no credentials or publishing authority.
CREATE TABLE social_account_selection (
    id TEXT PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    brand_key TEXT NOT NULL,
    provider TEXT NOT NULL CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok')),
    provider_account_id TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (brand_key, firm_id) REFERENCES brand(brand_key, firm_id),
    UNIQUE (brand_key, provider)
);
