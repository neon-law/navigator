-- Inbox-process state: the route the classification layer assigned and when
-- a process run recorded it. NULL for messages that were never processed.
ALTER TABLE message ADD COLUMN route TEXT CHECK (route IS NULL OR route IN ('nef', 'claims_agent', 'chambers', 'other'));
ALTER TABLE message ADD COLUMN processed_at TEXT;
