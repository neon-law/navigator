-- Migration: 20261105120000_inbox_conversation_person_id.sql
-- The Person the Conversation's inbound email sender resolves to. Written
-- at ingest — alias-aware through `person_mailbox` (the sender row is
-- resolved, or created, in the same engine before the conversation lands),
-- never by an address-book claim — and backfilled for rows that predate
-- the column. Identity only: automation never writes relationship or
-- entity_role edges from correspondence, and a lawyer- or clerk-tier
-- sender never links as an inbound counterparty. See the `Inbox` glossary
-- term.
ALTER TABLE inbox_conversation ADD COLUMN person_id UUID REFERENCES person(id);

-- Migration: 20261105130000_index_message_provider_message_id.sql
-- Email ingest threads a delivery into the Conversation of a Message it names in
-- In-Reply-To or References, looked up by that Message's RFC 5322 Message-ID.
CREATE INDEX message_provider_message_id ON message (provider_message_id) WHERE provider_message_id IS NOT NULL;

-- Migration: 20261105140000_entity_metadata.sql
ALTER TABLE entity ADD COLUMN metadata JSONB CHECK (metadata IS NULL OR jsonb_typeof(metadata) = 'object');

-- Migration: 20261105150000_create_esignature_webhook_journal.sql
-- The e-signature webhook journal: every verified, parseable webhook receipt the
-- provider delivered, journaled before its consumption. Journal-first is what makes
-- delivery recoverable — a death mid-consumption leaves the receipt replayable, and
-- the boot-time replay sweep drains unconsumed receipts oldest-first. `consumed_at`
-- is NULL until consumption succeeds; `attempts` and `last_error` record what a
-- failed replay did without halting the sweep. The provider's redelivery of the same
-- event lands on the same row, so the unique event index is the deduplication.
CREATE TABLE IF NOT EXISTS "esignature_webhook_journal" (
    "attempts" BIGINT NOT NULL,
    "consumed_at" TIMESTAMPTZ,
    "id" UUID NOT NULL PRIMARY KEY,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "last_error" TEXT,
    "payload" TEXT NOT NULL,
    "provider" TEXT NOT NULL,
    "provider_id" TEXT NOT NULL,
    "status" TEXT NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS esignature_webhook_journal_event
    ON "esignature_webhook_journal" ("provider", "provider_id", "status");
CREATE INDEX IF NOT EXISTS esignature_webhook_journal_pending
    ON "esignature_webhook_journal" ("consumed_at", "inserted_at");

