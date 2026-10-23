-- A verified Project party retained for conflict checks. This row is not
-- participation and conveys no access to the Project.
CREATE TABLE project_reference (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES project(id),
    person_id UUID REFERENCES person(id),
    entity_id UUID REFERENCES entity(id),
    reason TEXT NOT NULL,
    CONSTRAINT project_reference_reason CHECK (reason IN ('counterparty_officer', 'registered_agent', 'signatory', 'adverse_party', 'counterparty_entity')),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK ((person_id IS NOT NULL) <> (entity_id IS NOT NULL))
);

CREATE UNIQUE INDEX project_reference_person_tie
    ON project_reference (project_id, person_id, reason)
    WHERE person_id IS NOT NULL;

CREATE UNIQUE INDEX project_reference_entity_tie
    ON project_reference (project_id, entity_id, reason)
    WHERE entity_id IS NOT NULL;
