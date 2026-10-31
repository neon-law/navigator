-- Net-new matter work tracking: assignable Project tasks with audited state
-- transitions. A Task is work someone does, with a workflow state; it is not a
-- Deadline (a docket obligation the date binds). See docs/glossary/task.md.
-- PostgreSQL-native, following the project_costs pattern: retained rows,
-- trigger-enforced audit events, immutable event log.
--
-- The composite foreign key below pins an assignee to a Person who already
-- holds a participation row on the same Project. A UNIQUE (project_id,
-- person_id) index is required for it, in the FK's column order.
CREATE UNIQUE INDEX person_project_role_project_person
    ON person_project_role (project_id, person_id);

CREATE TABLE project_task (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    title TEXT NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 300),
    body TEXT CHECK (body IS NULL OR length(btrim(body)) BETWEEN 1 AND 2000),
    assignee_id UUID,
    due_on DATE,
    state TEXT NOT NULL CHECK (state IN ('todo', 'in_progress', 'done')),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, assignee_id)
        REFERENCES person_project_role (project_id, person_id)
);
CREATE INDEX project_task_project ON project_task (project_id, state, due_on, id);
CREATE INDEX project_task_assignee ON project_task (assignee_id);

CREATE TABLE project_task_events (
    id UUID PRIMARY KEY,
    project_task_id UUID NOT NULL REFERENCES project_task(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('created', 'assignee_changed', 'state_changed')),
    before_state JSONB,
    after_state JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX project_task_events_task_time ON project_task_events (project_task_id, occurred_at, id);

CREATE FUNCTION enforce_project_task_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
        event_kind_value TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Project tasks are retained; a state transition, never a delete';
    END IF;
    actor_id := nullif(current_setting('navigator.project_task_actor', true), '')::UUID;
    event_kind_value := current_setting('navigator.project_task_event_kind', true);
    IF actor_id IS NULL OR event_kind_value NOT IN ('assignee_changed', 'state_changed') THEN
        RAISE EXCEPTION 'Project task updates require an actor and audit event';
    END IF;
    INSERT INTO project_task_events(id,project_task_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,event_kind_value,to_jsonb(OLD),to_jsonb(NEW),now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_task_audit_update
    BEFORE UPDATE OR DELETE ON project_task
    FOR EACH ROW EXECUTE FUNCTION enforce_project_task_audit();

CREATE FUNCTION record_project_task_creation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
BEGIN
    actor_id := nullif(current_setting('navigator.project_task_actor', true), '')::UUID;
    IF actor_id IS NULL OR current_setting('navigator.project_task_event_kind', true) IS DISTINCT FROM 'created' THEN
        RAISE EXCEPTION 'Project task creation requires an actor and audit event';
    END IF;
    INSERT INTO project_task_events(id,project_task_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,'created',NULL,to_jsonb(NEW),now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_task_audit_create
    AFTER INSERT ON project_task
    FOR EACH ROW EXECUTE FUNCTION record_project_task_creation();

CREATE FUNCTION reject_project_task_event_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Project task audit events are immutable';
END;
$$;
CREATE TRIGGER project_task_events_immutable
    BEFORE UPDATE OR DELETE ON project_task_events
    FOR EACH ROW EXECUTE FUNCTION reject_project_task_event_mutation();
