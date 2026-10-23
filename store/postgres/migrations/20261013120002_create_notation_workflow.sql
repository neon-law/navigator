-- A Notation, its append-only Notation Event journal, and the append-only
-- answers its questionnaire collects, shaped as their SurrealDB sources so
-- the workflow mirror copies them one to one. Every id is a UUIDv7 minted
-- by the writer, so `ORDER BY id` is insertion order: the newest event for a
-- `(notation_id, machine_kind)` pair is the current state, and the newest
-- answer for a question, state, and respondent wins.
--
-- `template_id` and `template_version_id` name Draft rows PostgreSQL does
-- not hold yet; they carry no foreign key until Template is ported.
CREATE TABLE notation (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    template_id UUID NOT NULL,
    package_id UUID REFERENCES notation_package(id),
    person_id UUID NOT NULL REFERENCES person(id),
    entity_id UUID REFERENCES entity(id),
    project_id UUID NOT NULL REFERENCES project(id),
    state TEXT NOT NULL,
    delivery TEXT NOT NULL DEFAULT 'embedded',
    questionnaire_snapshot JSONB,
    git_commit_sha TEXT,
    template_source_commit_sha TEXT,
    template_body_sha256 TEXT CHECK (template_body_sha256 ~ '^[0-9a-f]{64}$'),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE notation_event (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notation_id UUID NOT NULL REFERENCES notation(id),
    acting_person_id UUID NOT NULL REFERENCES person(id),
    template_version_id UUID NOT NULL,
    machine_kind TEXT NOT NULL,
    from_state TEXT NOT NULL,
    to_state TEXT NOT NULL,
    condition TEXT NOT NULL,
    payload TEXT,
    recorded_at TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE answer (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    question_id UUID NOT NULL REFERENCES question(id),
    person_id UUID NOT NULL REFERENCES person(id),
    notation_id UUID REFERENCES notation(id),
    state_name TEXT,
    value JSONB NOT NULL,
    source TEXT NOT NULL DEFAULT 'lawyer' CHECK (source IN ('lawyer', 'client', 'extracted')),
    authored_by_person_id UUID REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX notation_project ON notation (project_id);
CREATE INDEX notation_person ON notation (person_id);
CREATE INDEX notation_template ON notation (template_id);
CREATE INDEX notation_event_machine ON notation_event (notation_id, machine_kind, id);
CREATE INDEX answer_notation ON answer (notation_id, id);
CREATE INDEX answer_question_person ON answer (question_id, person_id);
