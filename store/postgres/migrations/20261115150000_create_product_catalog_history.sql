-- Preserve historical billing catalog rows by their original UUID.
CREATE TABLE product (
    id UUID PRIMARY KEY,
    account_code TEXT NOT NULL,
    active BOOLEAN NOT NULL,
    billing_kind TEXT NOT NULL,
    cadence TEXT NOT NULL,
    code TEXT NOT NULL,
    currency TEXT NOT NULL,
    display_name TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    list_price_cents BIGINT NOT NULL,
    matter_close_template_code TEXT,
    retainer_template_code TEXT,
    updated_at TIMESTAMPTZ NOT NULL,
    xero_item_code TEXT
);
