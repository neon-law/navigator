-- Physical mail destinations and pieces of mail. SurrealDB remains the
-- source of truth while these tables receive bounded best-effort mirrors.
CREATE TABLE mailroom (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    address_id UUID NOT NULL REFERENCES address(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE letter (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    mailroom_id UUID NOT NULL REFERENCES mailroom(id),
    project_id UUID,
    direction TEXT NOT NULL CHECK (direction IN ('incoming', 'outgoing')),
    sender TEXT NOT NULL,
    recipient TEXT NOT NULL,
    summary TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX letter_project_id ON letter (project_id);
