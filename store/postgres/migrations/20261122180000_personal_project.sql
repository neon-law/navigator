-- A personal Project belongs to its self-serve owner. Existing firm matters retain their opening rules.
ALTER TABLE project ADD COLUMN self_serve BOOLEAN NOT NULL DEFAULT false;
ALTER TABLE project ADD CONSTRAINT personal_project_owner CHECK (NOT self_serve OR client_dri_person_id IS NOT NULL);

-- Adding another participant to the owner's personal Project would expose every business in it.
CREATE FUNCTION personal_project_participant() RETURNS TRIGGER LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM project p WHERE p.id=NEW.project_id AND p.self_serve
        AND (p.client_dri_person_id<>NEW.person_id OR NEW.participation<>'client' OR NEW.is_lawyer_dri OR NOT NEW.is_client_dri)) THEN
        RAISE EXCEPTION 'personal Project requires owner participation' USING ERRCODE='check_violation';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER personal_project_participant BEFORE INSERT OR UPDATE ON person_project_role
    FOR EACH ROW EXECUTE FUNCTION personal_project_participant();

CREATE TABLE notation_project_transfer (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    notation_id UUID NOT NULL REFERENCES notation(id) ON DELETE CASCADE,
    source_project_id UUID NOT NULL REFERENCES project(id),
    destination_project_id UUID NOT NULL REFERENCES project(id),
    person_id UUID NOT NULL REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (notation_id, destination_project_id),
    CHECK (source_project_id <> destination_project_id)
);
