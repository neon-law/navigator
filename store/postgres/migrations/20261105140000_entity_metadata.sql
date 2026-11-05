ALTER TABLE entity ADD COLUMN metadata JSONB CHECK (metadata IS NULL OR jsonb_typeof(metadata) = 'object');
