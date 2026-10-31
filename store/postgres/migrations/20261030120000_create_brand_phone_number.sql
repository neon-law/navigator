-- Migration: brand_phone_number — the Firm's Twilio receiving numbers.
-- Born in PostgreSQL: the webhook resolution spine reads it before any
-- Surreal record is touched. No foreign keys on purpose — brand_key and the
-- secret reference resolve against mirrors that can lag a registration;
-- intake fails closed when either does not resolve.
CREATE TABLE IF NOT EXISTS brand_phone_number (
    number_e164 TEXT PRIMARY KEY
        CHECK (number_e164 ~ '^\+[0-9]{8,15}$'),
    brand_key TEXT NOT NULL
        CHECK (brand_key = trim(brand_key) AND brand_key <> '' AND length(brand_key) <= 100),
    twilio_account_sid TEXT NOT NULL
        CHECK (twilio_account_sid = trim(twilio_account_sid) AND twilio_account_sid <> '' AND length(twilio_account_sid) <= 64),
    auth_token_secret_ref UUID NOT NULL,
    channel_kinds TEXT NOT NULL
        CHECK (channel_kinds IN ('sms', 'voice', 'both')),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
