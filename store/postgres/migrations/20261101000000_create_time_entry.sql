-- Attributed attorney time on a matter, with the hourly rate snapshotted at
-- creation so later fee-structure edits can never reprice captured work (the
-- notation_meter_entry precedent). Minutes are integers, money is integer
-- cents, never floats. A Time Entry is work done and priced; a Project Task
-- is work still to do. See docs/glossary/time-entry.md.
-- PostgreSQL-native, following the project_tasks pattern: retained rows,
-- trigger-enforced audit events, immutable event log.

CREATE TABLE time_entry (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    -- The attorney who performed the work: a Person already holding a
    -- participation row on the same Project, refused at the database too.
    attorney_id UUID NOT NULL,
    performed_on DATE NOT NULL,
    minutes INTEGER NOT NULL CHECK (minutes BETWEEN 1 AND 1440),
    description TEXT NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 2000),
    billable BOOLEAN NOT NULL,
    -- Frozen at creation; a later rate edit never touches a captured entry.
    rate_cents_per_hour INTEGER NOT NULL CHECK (rate_cents_per_hour >= 0),
    source TEXT NOT NULL CHECK (source IN ('timer', 'manual')),
    billing_status TEXT NOT NULL CHECK (billing_status IN ('unbilled', 'invoiced', 'written_off')),
    xero_invoice_id TEXT,
    written_off_reason TEXT CHECK (written_off_reason IS NULL OR length(btrim(written_off_reason)) BETWEEN 1 AND 1000),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (project_id, attorney_id)
        REFERENCES person_project_role (project_id, person_id),
    CONSTRAINT time_entry_status_evidence CHECK (
        (billing_status = 'unbilled' AND xero_invoice_id IS NULL AND written_off_reason IS NULL) OR (billing_status = 'invoiced' AND xero_invoice_id IS NOT NULL AND written_off_reason IS NULL) OR (billing_status = 'written_off' AND xero_invoice_id IS NULL AND written_off_reason IS NOT NULL)
    )
);
CREATE INDEX time_entry_project ON time_entry (project_id, billing_status, performed_on, id);
CREATE INDEX time_entry_attorney ON time_entry (attorney_id);

CREATE TABLE time_entry_events (
    id UUID PRIMARY KEY,
    time_entry_id UUID NOT NULL REFERENCES time_entry(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('created', 'invoiced', 'written_off')),
    before_state JSONB,
    after_state JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX time_entry_events_entry_time ON time_entry_events (time_entry_id, occurred_at, id);

CREATE FUNCTION enforce_time_entry_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
        event_kind_value TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Time entries are retained; a write-off, never a delete';
    END IF;
    actor_id := nullif(current_setting('navigator.time_entry_actor', true), '')::UUID;
    event_kind_value := current_setting('navigator.time_entry_event_kind', true);
    IF actor_id IS NULL OR event_kind_value NOT IN ('invoiced', 'written_off') THEN
        RAISE EXCEPTION 'Time entry updates require an actor and audit event';
    END IF;
    INSERT INTO time_entry_events(id,time_entry_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,event_kind_value,to_jsonb(OLD),to_jsonb(NEW),now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER time_entry_audit_update
    BEFORE UPDATE OR DELETE ON time_entry
    FOR EACH ROW EXECUTE FUNCTION enforce_time_entry_audit();

CREATE FUNCTION record_time_entry_creation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
BEGIN
    actor_id := nullif(current_setting('navigator.time_entry_actor', true), '')::UUID;
    IF actor_id IS NULL OR current_setting('navigator.time_entry_event_kind', true) IS DISTINCT FROM 'created' THEN
        RAISE EXCEPTION 'Time entry creation requires an actor and audit event';
    END IF;
    INSERT INTO time_entry_events(id,time_entry_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,'created',NULL,to_jsonb(NEW),now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER time_entry_audit_create
    AFTER INSERT ON time_entry
    FOR EACH ROW EXECUTE FUNCTION record_time_entry_creation();

CREATE FUNCTION reject_time_entry_event_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Time entry audit events are immutable';
END;
$$;
CREATE TRIGGER time_entry_events_immutable
    BEFORE UPDATE OR DELETE ON time_entry_events
    FOR EACH ROW EXECUTE FUNCTION reject_time_entry_event_mutation();
