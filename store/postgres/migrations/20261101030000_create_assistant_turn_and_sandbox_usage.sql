-- SurrealDB remains the write authority; these tables are an opt-in mirror.

-- One ordered entry in a subscriber's Assistant Chat — the glossary's
-- Assistant Turn. A prompt the subscriber asked, a reply the assistant
-- gave, or a tool call and its result. Every row belongs to exactly one
-- Person and no other surface reads the table.
CREATE TABLE assistant_turn (
    id UUID PRIMARY KEY,
    person_id UUID NOT NULL REFERENCES person (id),
    kind TEXT NOT NULL CHECK (kind IN ('prompt', 'reply', 'tool_call', 'tool_result')),
    content TEXT NOT NULL,
    tool_name TEXT,
    input_tokens BIGINT,
    output_tokens BIGINT,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX assistant_turn_owner ON assistant_turn (person_id);

-- A subscriber's monthly Legal Sandbox credit — the glossary's Sandbox
-- Usage. One row per Person and billing period; amounts in cents,
-- matching the billing contract's integer-money rule.
CREATE TABLE sandbox_usage (
    id UUID PRIMARY KEY,
    person_id UUID NOT NULL REFERENCES person (id),
    period TEXT NOT NULL,
    granted_cents BIGINT NOT NULL CHECK (granted_cents >= 0),
    spent_cents BIGINT NOT NULL CHECK (spent_cents >= 0),
    overage BOOLEAN NOT NULL DEFAULT false,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (person_id, period)
);
CREATE INDEX sandbox_usage_period ON sandbox_usage (period, overage);
