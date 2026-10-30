-- LAW-238 D1: LinkedIn joins the modeled Inbox channels, and Messages gain
-- the object-storage key of their verbatim import payload. Imports arrive
-- from the member's own data export, not a provider webhook; the dedup and
-- threading keys are unchanged. The drop+re-add widens the inline channel
-- CHECK the create_inbox migration declared.
DO $$
BEGIN
    ALTER TABLE inbox_conversation DROP CONSTRAINT inbox_conversation_channel_check;
END $$;
ALTER TABLE inbox_conversation ADD CONSTRAINT inbox_conversation_channel_check CHECK (channel IN ('email', 'sms', 'facebook', 'instagram', 'threads', 'youtube', 'tiktok', 'linkedin'));
ALTER TABLE message ADD COLUMN raw_storage_key TEXT;
