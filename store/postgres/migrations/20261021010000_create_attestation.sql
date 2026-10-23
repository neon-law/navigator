CREATE TABLE attestation (
    id UUID PRIMARY KEY,
    notation_id UUID NOT NULL REFERENCES notation(id) ON DELETE RESTRICT,
    chain TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    status TEXT NOT NULL,
    pda TEXT,
    tx_signature TEXT,
    firm_wallet TEXT,
    client_wallet TEXT,
    recorded_at TIMESTAMPTZ,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
