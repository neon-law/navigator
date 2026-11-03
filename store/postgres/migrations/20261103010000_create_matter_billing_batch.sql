-- A Matter Billing Batch is one invoice intent: the frozen request (line
-- items, reference, consumed row ids) persisted BEFORE the provider call, so
-- a retry resumes the same batch under the same idempotency key — the batch
-- id itself — and a later billing batch composes a new invoice instead of
-- silently re-minting the first one. Rows the batch consumes are reserved
-- while the batch is pending; reservation blocks write-offs and cost
-- resolution so billed work cannot be removed behind the provider's back.
-- PostgreSQL-native, following the project_costs pattern: retained rows,
-- trigger-enforced audit events.

CREATE TABLE matter_billing_batch (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    status TEXT NOT NULL CHECK (status IN ('pending', 'completed')),
    reference TEXT NOT NULL CHECK (length(btrim(reference)) BETWEEN 1 AND 200),
    contact_name TEXT NOT NULL CHECK (length(btrim(contact_name)) > 0),
    contact_email TEXT NOT NULL CHECK (length(btrim(contact_email)) > 0),
    first_attempted_at TIMESTAMPTZ,
    line_items JSONB NOT NULL,
    total_cents BIGINT NOT NULL CHECK (total_cents > 0),
    currency TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    entry_ids UUID[] NOT NULL,
    cost_ids UUID[] NOT NULL,
    -- The provider invoice id, saved the moment a raise returns so a resume
    -- reuses the exact invoice instead of re-raising; NULL until then.
    xero_invoice_id TEXT,
    -- The flat fee the batch bills, when the matter carries one; NULL bills
    -- time and costs only.
    flat_fee_cents BIGINT,
    actor_id UUID NOT NULL REFERENCES person(id),
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
-- A matter holds at most one open invoice intent: the pending batch is the
-- one a retry resumes; the next batch composes only after it completes. The
-- database enforces what the handler assumes.
CREATE INDEX matter_billing_batch_pending ON matter_billing_batch (project_id, status);
CREATE UNIQUE INDEX matter_billing_batch_one_pending
    ON matter_billing_batch (project_id) WHERE status = 'pending';

-- Reservation: work drawn into a pending batch is no longer 'unbilled', but
-- it is not yet 'invoiced' either. Reserved rows carry no xero id and no
-- write-off reason, so the status-evidence constraint gains a third arm.
ALTER TABLE time_entry DROP CONSTRAINT time_entry_billing_status_check;
ALTER TABLE time_entry ADD CONSTRAINT time_entry_billing_status_check CHECK (billing_status IN ('unbilled', 'reserved', 'invoiced', 'written_off'));
ALTER TABLE time_entry DROP CONSTRAINT time_entry_status_evidence;
ALTER TABLE time_entry ADD CONSTRAINT time_entry_status_evidence CHECK ( (billing_status = 'unbilled' AND xero_invoice_id IS NULL AND written_off_reason IS NULL) OR (billing_status = 'reserved' AND xero_invoice_id IS NULL AND written_off_reason IS NULL) OR (billing_status = 'invoiced' AND xero_invoice_id IS NOT NULL AND written_off_reason IS NULL) OR (billing_status = 'written_off' AND xero_invoice_id IS NULL AND written_off_reason IS NOT NULL) );
ALTER TABLE time_entry_events DROP CONSTRAINT time_entry_events_event_kind_check;
ALTER TABLE time_entry_events ADD CONSTRAINT time_entry_events_event_kind_check CHECK (event_kind IN ('created', 'reserved', 'unreserved', 'invoiced', 'written_off'));

-- The audit trigger whitelists the event kinds an update may carry; the
-- reservation transitions are audited like every other mutation.
CREATE OR REPLACE FUNCTION enforce_time_entry_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
        event_kind_value TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Time entries are retained; a write-off, never a delete';
    END IF;
    actor_id := nullif(current_setting('navigator.time_entry_actor', true), '')::UUID;
    event_kind_value := current_setting('navigator.time_entry_event_kind', true);
    IF actor_id IS NULL OR event_kind_value NOT IN ('reserved', 'unreserved', 'invoiced', 'written_off') THEN
        RAISE EXCEPTION 'Time entry updates require an actor and audit event';
    END IF;
    INSERT INTO time_entry_events(id,time_entry_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,event_kind_value,to_jsonb(OLD),to_jsonb(NEW),now());
    RETURN NEW;
END;
$$;

ALTER TABLE project_costs DROP CONSTRAINT project_costs_billing_status_check;
ALTER TABLE project_costs ADD CONSTRAINT project_costs_billing_status_check CHECK (billing_status IN ('unbilled', 'reserved', 'invoiced', 'resolved'));
ALTER TABLE project_cost_events DROP CONSTRAINT project_cost_events_event_kind_check;
ALTER TABLE project_cost_events ADD CONSTRAINT project_cost_events_event_kind_check CHECK (event_kind IN ('created', 'amount_confirmed', 'reserved', 'unreserved', 'invoice_linked', 'resolved'));

CREATE OR REPLACE FUNCTION enforce_project_cost_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE actor_id UUID;
        event_kind_value TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Project costs are retained; resolve a cost instead of deleting it';
    END IF;
    actor_id := nullif(current_setting('navigator.project_cost_actor', true), '')::UUID;
    event_kind_value := current_setting('navigator.project_cost_event_kind', true);
    IF actor_id IS NULL OR event_kind_value NOT IN ('amount_confirmed', 'reserved', 'unreserved', 'invoice_linked', 'resolved') THEN
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
