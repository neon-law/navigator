-- Preserve source uniqueness at the PostgreSQL command boundary. Historical
-- record links remain importable without requiring their parent to exist.
ALTER TABLE expunge_record ADD CONSTRAINT expunge_record_category CHECK (category IN ('privilege', 'sealing', 'client_request', 'retention'));
ALTER TABLE expunge_request ADD CONSTRAINT expunge_request_status CHECK (status IN ('pending', 'authorized', 'denied'));
CREATE UNIQUE INDEX efiling_transaction_id ON efiling_transactions(project_id, transaction_id);
ALTER TABLE efiling_transactions ADD CONSTRAINT efiling_transaction_status CHECK (status IN ('submitted', 'accepted', 'rejected', 'unknown'));
CREATE UNIQUE INDEX social_oauth_attempt_state ON social_oauth_attempt(state_hash);
ALTER TABLE social_oauth_attempt ADD CONSTRAINT social_oauth_provider CHECK (provider IN ('facebook', 'instagram', 'threads', 'youtube', 'linkedin', 'x', 'tiktok'));
-- Optional citation pins preserve absent SQL NULL separately from explicit JSON null.
ALTER TABLE citation ALTER COLUMN source_pin DROP NOT NULL;
ALTER TABLE citation ALTER COLUMN draft_pin DROP NOT NULL;
