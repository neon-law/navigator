-- Firm-authored Case Stage vocabulary and audited per-matter transitions.
-- A Case Stage is where the matter sits in the Firm's own workflow; it is
-- distinct from project.status (the lifecycle: open/closed/archived) and
-- case.status (per-forum: open/stayed/closed). See docs/glossary/case-stage.md.
-- PostgreSQL-native, following the project_tasks pattern: a retained,
-- immutable history table whose writes the database itself polices.

CREATE TABLE stage_definition (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    label TEXT NOT NULL CHECK (length(btrim(label)) BETWEEN 1 AND 100),
    position INTEGER NOT NULL CHECK (position >= 0),
    case_type TEXT CHECK (case_type IS NULL OR length(btrim(case_type)) BETWEEN 1 AND 100),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (firm_id, position)
);
CREATE INDEX stage_definition_firm ON stage_definition (firm_id, position);

-- The per-matter history: one append-only row per transition. The current
-- stage is the newest row; there is no mutable "current stage" column to
-- drift from the history.
CREATE TABLE stage_event (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    from_stage_id UUID REFERENCES stage_definition(id),
    to_stage_id UUID NOT NULL REFERENCES stage_definition(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    note TEXT CHECK (note IS NULL OR length(btrim(note)) BETWEEN 1 AND 500),
    occurred_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX stage_event_project_time ON stage_event (project_id, occurred_at, id);

-- Every write is validated where the data lives: the target (and origin) must
-- come from the matter Firm's own vocabulary, and the chain must continue
-- from the current stage. The advisory lock serializes concurrent transitions
-- on one matter so two writers cannot both read the same "current" stage.
CREATE FUNCTION enforce_stage_event_validity() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE project_firm UUID;
        current_stage UUID;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtext(NEW.project_id::text));
    SELECT firm_id INTO project_firm FROM project WHERE id = NEW.project_id;
    IF project_firm IS NULL THEN
        RAISE EXCEPTION 'A Case Stage transition requires a Project that belongs to a Firm';
    END IF;
    IF EXISTS (
        SELECT 1 FROM stage_definition d
        WHERE d.id = NEW.to_stage_id AND d.firm_id IS DISTINCT FROM project_firm
    ) OR (
        NEW.from_stage_id IS NOT NULL AND EXISTS (
            SELECT 1 FROM stage_definition d
            WHERE d.id = NEW.from_stage_id AND d.firm_id IS DISTINCT FROM project_firm
        )
    ) THEN
        RAISE EXCEPTION 'A Case Stage must come from the matter Firm''s own vocabulary';
    END IF;
    SELECT to_stage_id INTO current_stage FROM stage_event
    WHERE project_id = NEW.project_id
    ORDER BY occurred_at DESC, id DESC
    LIMIT 1;
    IF current_stage IS DISTINCT FROM NEW.from_stage_id THEN
        RAISE EXCEPTION 'A Case Stage transition must continue from the matter''s current stage';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER stage_event_validity
    BEFORE INSERT ON stage_event
    FOR EACH ROW EXECUTE FUNCTION enforce_stage_event_validity();

CREATE FUNCTION reject_stage_event_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Case Stage history is immutable; a new transition, never an edit';
END;
$$;
CREATE TRIGGER stage_event_immutable
    BEFORE UPDATE OR DELETE ON stage_event
    FOR EACH ROW EXECUTE FUNCTION reject_stage_event_mutation();
