-- Project-scoped records and a client Entity's playbooks, mirrored from
-- SurrealDB under their record UUIDs.
CREATE TABLE project_module (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    module TEXT NOT NULL CHECK (module IN ('litigation', 'cap_table', 'estate', 'deadlines')),
    enabled_at TIMESTAMPTZ NOT NULL,
    enabled_by_person_id UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, module)
);

CREATE TABLE statutory_deadline (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    trigger_on DATE NOT NULL,
    due_on DATE NOT NULL,
    statute TEXT NOT NULL,
    source TEXT NOT NULL,
    status TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, kind, trigger_on, source)
);

CREATE TABLE testimonial (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    person_id UUID NOT NULL REFERENCES person(id),
    quote TEXT NOT NULL,
    attribution_label TEXT,
    consented_at TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    display_order INTEGER NOT NULL DEFAULT 0,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, person_id, quote)
);

CREATE TABLE playbook (
    id UUID PRIMARY KEY,
    entity_id UUID NOT NULL REFERENCES entity(id),
    name TEXT NOT NULL,
    positions JSONB,
    active BOOLEAN NOT NULL DEFAULT true,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (entity_id, name)
);
