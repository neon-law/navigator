-- Migration: billed_call — billed time for completed voice calls.
-- Born in PostgreSQL: one idempotent row per CallSid, written only from
-- Twilio's final CallStatus=completed event. No foreign keys on purpose —
-- the conversation is authored in the store the webhook serves and project
-- attribution happens at intake; either can lag the completed event, and
-- billing must not fail closed on a mirror race. `tenths` is derived from
-- `duration_seconds` in the store and stored beside it so the audit trail
-- shows the billed units as they landed.
CREATE TABLE IF NOT EXISTS billed_call (
    call_sid TEXT PRIMARY KEY
        CHECK (call_sid = trim(call_sid) AND call_sid <> '' AND length(call_sid) <= 64),
    conversation_id UUID NOT NULL,
    firm_id UUID NOT NULL,
    project_id UUID,
    duration_seconds BIGINT NOT NULL
        CHECK (duration_seconds >= 0),
    tenths BIGINT NOT NULL
        CHECK (tenths >= 0),
    call_started_at TIMESTAMPTZ NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX billed_call_firm_started ON billed_call(firm_id, call_started_at);
