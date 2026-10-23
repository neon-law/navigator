-- Project-scoped third-party costs; Xero remains the invoice system of record.
CREATE UNIQUE INDEX asset_project_id_id ON asset(project_id,id);
CREATE TABLE project_costs (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    occurred_on DATE NOT NULL,
    payee TEXT NOT NULL CHECK (length(btrim(payee)) BETWEEN 1 AND 300),
    description TEXT NOT NULL CHECK (length(btrim(description)) BETWEEN 1 AND 2000),
    amount_cents BIGINT NOT NULL CHECK (amount_cents > 0),
    currency TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    amount_status TEXT NOT NULL CHECK (amount_status IN ('estimated', 'confirmed')),
    billing_status TEXT NOT NULL CHECK (billing_status IN ('unbilled', 'invoiced', 'resolved')),
    receipt_document_id UUID NOT NULL,
    xero_invoice_id TEXT,
    idempotency_payload JSONB NOT NULL,
    idempotency_key TEXT NOT NULL CHECK (length(btrim(idempotency_key)) BETWEEN 1 AND 200),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (project_id, idempotency_key),
    UNIQUE (project_id, receipt_document_id),
    FOREIGN KEY (project_id,receipt_document_id) REFERENCES asset(project_id,id),
    CHECK ((billing_status = 'invoiced') = (xero_invoice_id IS NOT NULL))
);
CREATE INDEX project_costs_project_date ON project_costs(project_id, occurred_on, id);

CREATE TABLE project_cost_events (
    id UUID PRIMARY KEY,
    project_cost_id UUID NOT NULL REFERENCES project_costs(id),
    actor_person_id UUID NOT NULL REFERENCES person(id),
    event_kind TEXT NOT NULL CHECK (event_kind IN ('created', 'amount_confirmed', 'invoice_linked', 'resolved')),
    before_state JSONB,
    after_state JSONB NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL
);
CREATE INDEX project_cost_events_cost_time ON project_cost_events(project_cost_id, occurred_at, id);

CREATE FUNCTION enforce_project_cost_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
        event_kind_value TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Project costs are retained; resolve a cost instead of deleting it';
    END IF;
    actor_id := nullif(current_setting('navigator.project_cost_actor', true), '')::UUID;
    event_kind_value := current_setting('navigator.project_cost_event_kind', true);
    IF actor_id IS NULL OR event_kind_value NOT IN ('amount_confirmed', 'invoice_linked', 'resolved') THEN
        RAISE EXCEPTION 'Project cost updates require an actor and audit event';
    END IF;
    INSERT INTO project_cost_events(id,project_cost_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,event_kind_value,to_jsonb(OLD),
        to_jsonb(NEW) || CASE WHEN nullif(current_setting('navigator.project_cost_event_detail', true), '') IS NOT NULL
            THEN jsonb_build_object('reason',current_setting('navigator.project_cost_event_detail', true))
            ELSE '{}'::jsonb END,now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_costs_audit_update
    BEFORE UPDATE OR DELETE ON project_costs
    FOR EACH ROW EXECUTE FUNCTION enforce_project_cost_audit();

CREATE FUNCTION record_project_cost_creation() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
BEGIN
    actor_id := nullif(current_setting('navigator.project_cost_actor', true), '')::UUID;
    IF actor_id IS NULL OR current_setting('navigator.project_cost_event_kind', true) IS DISTINCT FROM 'created' THEN
        RAISE EXCEPTION 'Project cost creation requires an actor and audit event';
    END IF;
    INSERT INTO project_cost_events(id,project_cost_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,'created',NULL,
        to_jsonb(NEW) || jsonb_build_object('request',NEW.idempotency_payload),now());
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_costs_audit_create
    AFTER INSERT ON project_costs
    FOR EACH ROW EXECUTE FUNCTION record_project_cost_creation();

CREATE FUNCTION reject_project_cost_event_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'Project cost audit events are immutable';
END;
$$;
CREATE TRIGGER project_cost_events_immutable
    BEFORE UPDATE OR DELETE ON project_cost_events
    FOR EACH ROW EXECUTE FUNCTION reject_project_cost_event_mutation();
