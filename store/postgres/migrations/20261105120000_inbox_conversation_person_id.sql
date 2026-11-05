-- The Person the Conversation's inbound email sender resolves to. Written
-- at ingest — alias-aware through `person_mailbox` (the sender row is
-- resolved, or created, in the same engine before the conversation lands),
-- never by an address-book claim — and backfilled for rows that predate
-- the column. Identity only: automation never writes relationship or
-- entity_role edges from correspondence, and a lawyer- or clerk-tier
-- sender never links as an inbound counterparty. See the `Inbox` glossary
-- term.
ALTER TABLE inbox_conversation ADD COLUMN person_id UUID REFERENCES person(id);
