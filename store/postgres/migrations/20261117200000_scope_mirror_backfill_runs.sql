-- Scoped enrollments share the existing run/checkpoint ledger and importer lock.
-- Global completion is deliberately distinct from family activation.
ALTER TABLE mirror_backfill_run ADD COLUMN scope TEXT NOT NULL DEFAULT 'global' CHECK (scope <> '');
ALTER TABLE mirror_backfill_run ADD COLUMN source_receipt JSONB;
ALTER TABLE mirror_backfill_run ADD COLUMN source_receipt_history JSONB NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof(source_receipt_history) = 'array');
ALTER TABLE mirror_backfill_run ADD CONSTRAINT mirror_backfill_scope_receipt CHECK ((scope = 'global' AND source_receipt IS NULL) OR (scope <> 'global' AND source_receipt IS NOT NULL AND jsonb_typeof(source_receipt) = 'object'));
ALTER TABLE mirror_backfill_checkpoint ADD COLUMN verification JSONB CHECK (verification IS NULL OR jsonb_typeof(verification) = 'object');
CREATE INDEX mirror_backfill_run_scope ON mirror_backfill_run (scope, status);
