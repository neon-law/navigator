-- When a Conversation was moved to the Inbox trash; NULL while it is in the
-- Inbox. Trash hides the Conversation from the Inbox lists and nothing else:
-- no row is removed, and a new inbound Message clears the stamp. See the
-- `Inbox Trash` glossary term.
ALTER TABLE inbox_conversation ADD COLUMN trashed_at TIMESTAMPTZ;
