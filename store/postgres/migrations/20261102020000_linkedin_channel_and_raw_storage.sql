-- Imported direct messages and scraper notices share the Inbox channel vocabulary.
DO $$ BEGIN
    ALTER TABLE inbox_conversation DROP CONSTRAINT IF EXISTS inbox_conversation_channel_check;
END $$;
ALTER TABLE inbox_conversation ADD CONSTRAINT inbox_conversation_channel_check CHECK (channel IN ('email', 'sms', 'facebook', 'instagram', 'threads', 'youtube', 'tiktok', 'linkedin', 'scraper', 'x', 'voice'));
ALTER TABLE message ADD COLUMN raw_storage_key TEXT;
