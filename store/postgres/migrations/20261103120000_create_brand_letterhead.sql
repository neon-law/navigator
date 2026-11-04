-- Migration: brand_letterhead — the letterhead a Brand frames its letters with.
-- Born in PostgreSQL with no Surreal source. One row per Brand, keyed by the
-- Brand's id rather than its key, so a deleted and re-created key never
-- inherits another Brand's letterhead. No foreign key on purpose: the Brand
-- row may still be authoritative in SurrealDB, so the writer authorizes
-- against the Brand before it writes. Every content column is optional; an
-- unset one falls back to the Brand's own name, website, and font when read.
CREATE TABLE IF NOT EXISTS brand_letterhead (
    brand_id UUID PRIMARY KEY,
    wordmark TEXT
        CHECK (wordmark IS NULL OR (wordmark = trim(wordmark) AND wordmark <> '' AND length(wordmark) <= 100)),
    mark TEXT NOT NULL DEFAULT 'wordmark'
        CHECK (mark IN ('wordmark', 'logo')),
    phone TEXT
        CHECK (phone IS NULL OR (phone = trim(phone) AND phone <> '' AND length(phone) <= 40)),
    email TEXT
        CHECK (email IS NULL OR (email = trim(email) AND email LIKE '%_@_%' AND length(email) <= 254)),
    site_url TEXT
        CHECK (site_url IS NULL OR site_url ~ '^https://[a-z0-9-]+(\.[a-z0-9-]+)+$'),
    postal_address TEXT
        CHECK (postal_address IS NULL OR (postal_address = trim(postal_address) AND postal_address <> '' AND length(postal_address) <= 500)),
    font_family TEXT
        CHECK (font_family IS NULL OR (font_family = trim(font_family) AND font_family <> '' AND length(font_family) <= 100)),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
