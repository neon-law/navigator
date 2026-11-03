-- Preserve Inbox channel storage across voice, social, and scraper ingestion.
ALTER TABLE inbox_conversation DROP CONSTRAINT inbox_conversation_channel_check;
ALTER TABLE inbox_conversation ADD CONSTRAINT inbox_conversation_channel_check CHECK (channel IN ('email', 'sms', 'voice', 'facebook', 'instagram', 'threads', 'youtube', 'tiktok', 'scraper', 'linkedin', 'x'));
