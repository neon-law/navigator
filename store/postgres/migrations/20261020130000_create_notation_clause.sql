-- Per-Notation custom prose, retained in render order with its author.
CREATE TABLE notation_clause (
    id UUID PRIMARY KEY,
    notation_id UUID NOT NULL REFERENCES notation(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    body_markdown TEXT NOT NULL,
    authored_by_person_id UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX notation_clause_notation ON notation_clause (notation_id, position, id);
