-- The signature, verification, and notarization tables hold the application's
-- authority for these rows once NAVIGATOR_POSTGRES_HOST is set, so the unique
-- semantics SurrealDB's schema enforces on the same tables must hold here too:
-- one envelope per (provider, provider_id) — the correlation key
-- `store::signatures::record_request` and `store::notarizations::record_request`
-- lose races through — and the notation and citation indexes the modules'
-- ordered reads use.
CREATE UNIQUE INDEX signature_provider_request
    ON "signature" (provider, provider_id);
CREATE INDEX signature_notation
    ON "signature" (notation_id, inserted_at);
CREATE UNIQUE INDEX notarization_provider_request
    ON "notarization" (provider, provider_id);
CREATE INDEX notarization_notation
    ON "notarization" (notation_id, inserted_at);
CREATE INDEX verification_citation
    ON "verification" (citation_id, id);
