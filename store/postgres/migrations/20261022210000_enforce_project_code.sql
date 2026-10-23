-- Project routes, repository bindings and document prefixes share this coordinate.
-- Keep the source's READONLY and reserved-route rules in the database itself.
ALTER TABLE project ADD CONSTRAINT project_code_reserved CHECK (code NOT IN ('new', 'closed'));

CREATE FUNCTION project_code_immutable() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.code IS DISTINCT FROM OLD.code THEN
        RAISE EXCEPTION 'Project code is immutable'
            USING ERRCODE = '23514', CONSTRAINT = 'project_code_immutable';
    END IF;
    RETURN NEW;
END $$;

CREATE TRIGGER project_code_immutable BEFORE UPDATE OF code ON project
    FOR EACH ROW EXECUTE FUNCTION project_code_immutable();
