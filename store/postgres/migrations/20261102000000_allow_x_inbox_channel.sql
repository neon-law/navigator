-- Retain the current Inbox channels while admitting X activity to the mirror.
ALTER TABLE inbox_conversation DROP CONSTRAINT inbox_conversation_channel_check;
ALTER TABLE inbox_conversation ADD CONSTRAINT inbox_conversation_channel_check CHECK (channel IN ('email', 'sms', 'facebook', 'instagram', 'threads', 'youtube', 'tiktok', 'scraper', 'x'));
