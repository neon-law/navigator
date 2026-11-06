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
