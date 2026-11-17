ALTER TABLE matter_page ADD COLUMN media JSONB;
ALTER TABLE evidence_share ADD COLUMN media JSONB NOT NULL DEFAULT '[]'::jsonb;
