-- The Firm's stage vocabulary on captured time (ENG-1269): a Time Entry
-- carries which canonical step the minutes belong to, so plan reporting can
-- show where an hour went. Nullable — a captured entry keeps its minutes
-- while the nightly project update nags for the missing stage.
--
-- The stage comes from the Firm's own `stage_definition` vocabulary, never a
-- hardcoded enum, and it must be the matter Firm's vocabulary: the trigger
-- proves the same-Firm join the way `contingency_term` proves a tier's stage.

ALTER TABLE time_entry ADD COLUMN stage UUID REFERENCES stage_definition (id);

-- Reclassification is visible: the audit event vocabulary learns a kind, and
-- the update audit below journals every change. The constraint and the audit
-- allowlist extend the billing batch's list — the reservation kinds stay —
-- and the constraint is re-added under its auto-assigned name with the
-- extended list.
ALTER TABLE time_entry_events DROP CONSTRAINT time_entry_events_event_kind_check;
ALTER TABLE time_entry_events ADD CONSTRAINT time_entry_events_event_kind_check CHECK (event_kind IN ('created', 'reserved', 'unreserved', 'invoiced', 'written_off', 'stage_changed'));

-- A stage must belong to the entry's Project's own Firm.
CREATE FUNCTION enforce_time_entry_stage_firm() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    project_firm UUID;
BEGIN
    IF NEW.stage IS NULL THEN
        RETURN NEW;
    END IF;
    SELECT firm_id INTO project_firm FROM project WHERE id = NEW.project_id;
    IF NOT EXISTS (
        SELECT 1 FROM stage_definition s
        WHERE s.id = NEW.stage AND s.firm_id = project_firm
    ) THEN
        RAISE EXCEPTION 'A time entry stage must come from the matter Firm''s own vocabulary';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER time_entry_stage_firm
    BEFORE INSERT OR UPDATE ON time_entry
    FOR EACH ROW EXECUTE FUNCTION enforce_time_entry_stage_firm();

-- The update audit extends to reclassification: `stage_changed` journals the
-- before and after states like an invoice or write-off, and refuses an event
-- that would record no change at all. Existing update rules are unchanged.
CREATE OR REPLACE FUNCTION enforce_time_entry_audit() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    actor_id UUID;
    event_kind_value TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Time entries are retained; a write-off, never a delete';
    END IF;
    actor_id := nullif(current_setting('navigator.time_entry_actor', true), '')::UUID;
    event_kind_value := current_setting('navigator.time_entry_event_kind', true);
    IF actor_id IS NULL
        OR event_kind_value NOT IN ('reserved', 'unreserved', 'invoiced', 'written_off', 'stage_changed')
    THEN
        RAISE EXCEPTION 'Time entry updates require an actor and audit event';
    END IF;
    IF event_kind_value = 'stage_changed' AND OLD.stage IS NOT DISTINCT FROM NEW.stage THEN
        RAISE EXCEPTION 'A stage_changed audit event requires a stage change';
    END IF;
    INSERT INTO time_entry_events(id,time_entry_id,actor_person_id,event_kind,before_state,after_state,occurred_at)
    VALUES(gen_random_uuid(),NEW.id,actor_id,event_kind_value,to_jsonb(OLD),to_jsonb(NEW),now());
    RETURN NEW;
END;
$$;
