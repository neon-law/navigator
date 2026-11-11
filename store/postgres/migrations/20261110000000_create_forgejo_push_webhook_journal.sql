-- The Forgejo push webhook journal: every verified push delivery journaled
-- before its consumption. Journal-first is what makes delivery recoverable —
-- a death mid-consumption leaves the receipt replayable, and the boot-time
-- replay sweep drains re-consumable receipts (pending or failed) oldest-first.
-- `delivered` and `skipped` are terminal; `failed` is re-consumable, and a
-- failed attempt names itself in `failure_detail`. The D2 analysis columns
-- stay NULL until the change analysis fills them, so a delivery crash after
-- analysis never re-reads the Forgejo API. A Forgejo redelivery re-sends the
-- same body under a new delivery id, so deduplication is content-based: the
-- unique event index is the (repository, ref updates) pair the store derives.
CREATE TABLE IF NOT EXISTS "forgejo_push_webhook_journal" (
    "id" UUID NOT NULL PRIMARY KEY,
    "delivered_at" TIMESTAMPTZ NOT NULL,
    "repo_full_name" TEXT NOT NULL,
    "ref_updates" JSONB NOT NULL,
    "event_key" TEXT NOT NULL,
    "payload" TEXT NOT NULL,
    "status" TEXT NOT NULL CHECK (status IN ('pending', 'analyzed', 'delivered', 'failed', 'skipped')),
    "failure_detail" TEXT,
    "change_id" TEXT,
    "author" TEXT,
    "description_summary" TEXT,
    "stats" JSONB,
    "conflict_signal" BOOLEAN,
    "inserted_at" TIMESTAMPTZ NOT NULL,
    "updated_at" TIMESTAMPTZ NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS forgejo_push_webhook_journal_event
    ON "forgejo_push_webhook_journal" ("event_key");
CREATE INDEX IF NOT EXISTS forgejo_push_webhook_journal_reconsumable
    ON "forgejo_push_webhook_journal" ("inserted_at")
    WHERE "status" IN ('pending', 'failed');
