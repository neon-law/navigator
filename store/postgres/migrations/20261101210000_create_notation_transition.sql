-- Target-only DBOS runtime ownership; ephemeral Notations need no domain row.
CREATE TABLE notation_transition (
    id UUID PRIMARY KEY,
    notation_id UUID NOT NULL,
    request_fingerprint TEXT NOT NULL,
    started BOOLEAN NOT NULL DEFAULT false,
    result JSONB,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT notation_transition_completion CHECK ((result IS NULL) = (completed_at IS NULL))
);
-- A pending operation itself is the durable owner, across both machine kinds.
CREATE UNIQUE INDEX notation_transition_pending ON notation_transition(notation_id) WHERE result IS NULL;
CREATE INDEX notation_transition_notation ON notation_transition(notation_id);
