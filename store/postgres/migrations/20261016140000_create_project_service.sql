-- Project-scoped proof that a filed Document reached a recipient.
CREATE TABLE project_service (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    document_id UUID NOT NULL REFERENCES asset(id),
    recipient TEXT NOT NULL,
    recipient_person_id UUID REFERENCES person(id),
    recipient_entity_id UUID REFERENCES entity(id),
    counsel_for TEXT,
    method TEXT NOT NULL CHECK (method IN ('personal', 'mail', 'certified_mail', 'overnight', 'e_service', 'electronic_by_agreement')),
    served_on DATE NOT NULL,
    address TEXT NOT NULL,
    declarant TEXT NOT NULL,
    declarant_person_id UUID REFERENCES person(id),
    letter_id UUID,
    sent_email_id UUID REFERENCES sent_email(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    CHECK (recipient_person_id IS NULL OR recipient_entity_id IS NULL)
);
CREATE INDEX project_service_project_document ON project_service (project_id, document_id);
