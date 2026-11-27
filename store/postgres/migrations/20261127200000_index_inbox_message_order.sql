CREATE INDEX message_conversation_occurred
    ON message (conversation_id, occurred_at DESC, id DESC);
