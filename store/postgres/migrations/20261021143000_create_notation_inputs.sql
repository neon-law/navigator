-- A Notation's ordered intake sources and a Project's unrun Notation drafts,
-- mirrored from SurrealDB under their record UUIDs. A source names its
-- Communication, never a copy of the message.
CREATE TABLE notation_source (
    id UUID PRIMARY KEY,
    notation_id UUID NOT NULL REFERENCES notation(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    communication_id UUID REFERENCES communication(id),
    source_ref TEXT,
    position INTEGER NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (notation_id, position)
);

CREATE INDEX notation_source_communication ON notation_source (communication_id);

CREATE TABLE notation_draft (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    slug TEXT NOT NULL,
    title TEXT NOT NULL,
    source TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX notation_draft_project ON notation_draft (project_id);
