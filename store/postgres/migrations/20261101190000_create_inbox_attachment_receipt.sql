-- Trusted clean-byte receipt for an unassigned Firm Inbox attachment.
-- A globally deduplicated bare Asset may be independently received by many Firms.
CREATE TABLE inbox_attachment_receipt (
    firm_id UUID NOT NULL REFERENCES firm(id),
    asset_id UUID NOT NULL REFERENCES asset(id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (firm_id, asset_id)
);
