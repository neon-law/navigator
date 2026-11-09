CREATE TABLE project_status_transition (
    sequence BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    from_status TEXT CHECK (from_status IN ('open', 'closed', 'archived')),
    to_status TEXT NOT NULL CHECK (to_status IN ('open', 'closed', 'archived')),
    effective_at TIMESTAMPTZ,
    recorded_at TIMESTAMPTZ,
    reason TEXT,
    attesting_person_id UUID,
    closed_at TEXT,
    closure_reason TEXT,
    CHECK (from_status IS NULL OR (effective_at IS NOT NULL AND recorded_at IS NOT NULL)),
    CHECK (to_status <> 'open' OR (closed_at IS NULL AND closure_reason IS NULL))
);

CREATE INDEX project_status_transition_project_sequence
    ON project_status_transition (project_id, sequence DESC);

-- The compatibility mirror may still supply a Project image. Once the ledger
-- owns its lifecycle, only a new transition can change these three columns.
CREATE FUNCTION project_status_project_lifecycle() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    latest project_status_transition;
BEGIN
    SELECT * INTO latest FROM project_status_transition
    WHERE project_id = NEW.id ORDER BY sequence DESC LIMIT 1;
    IF FOUND THEN
        NEW.status := latest.to_status;
        NEW.closed_at := latest.closed_at;
        NEW.closure_reason := latest.closure_reason;
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_status_project_lifecycle
    BEFORE UPDATE ON project FOR EACH ROW EXECUTE FUNCTION project_status_project_lifecycle();

CREATE FUNCTION project_status_apply_transition() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    UPDATE project SET status = NEW.to_status, closed_at = NEW.closed_at,
        closure_reason = NEW.closure_reason WHERE id = NEW.project_id;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_status_apply_transition
    AFTER INSERT ON project_status_transition
    FOR EACH ROW EXECUTE FUNCTION project_status_apply_transition();

CREATE FUNCTION project_status_guard_transition() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    current_status TEXT;
    latest_sequence BIGINT;
BEGIN
    SELECT status INTO STRICT current_status FROM project WHERE id = NEW.project_id FOR UPDATE;
    SELECT max(sequence) INTO latest_sequence FROM project_status_transition WHERE project_id = NEW.project_id;
    IF NEW.from_status IS NULL THEN
        IF latest_sequence IS NOT NULL OR NEW.to_status <> current_status THEN
            RAISE EXCEPTION 'Project lifecycle baseline must reflect the current Project';
        END IF;
    ELSIF NEW.from_status <> current_status
        OR (current_status = 'archived' AND NEW.to_status <> 'archived')
        OR NEW.sequence <= latest_sequence THEN
        RAISE EXCEPTION 'Project lifecycle transition conflicts with the current Project';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER project_status_guard_transition
    BEFORE INSERT ON project_status_transition
    FOR EACH ROW EXECUTE FUNCTION project_status_guard_transition();

CREATE FUNCTION project_status_protect_history() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND NOT EXISTS (SELECT 1 FROM project WHERE id = OLD.project_id) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'Project lifecycle history is append-only';
END;
$$;

CREATE TRIGGER project_status_protect_history
    BEFORE UPDATE OR DELETE ON project_status_transition
    FOR EACH ROW EXECUTE FUNCTION project_status_protect_history();
