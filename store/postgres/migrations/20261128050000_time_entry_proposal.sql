-- Time a lawyer has not yet approved (LAW-583, LAW-584). A Time Entry
-- Proposal is recorded from the CLI or proposed from a Project's dated record
-- (a Communication, a filed document, a completed Deadline, an e-filing). It
-- prices nothing: approval creates the Time Entry and freezes its rate.
-- See docs/glossary/time-entry-proposal.md.
CREATE TABLE time_entry_proposal (
    id UUID PRIMARY KEY,
    project_id UUID NOT NULL REFERENCES project(id),
    timekeeper_id UUID NOT NULL,
    performed_on DATE NOT NULL,
    minutes INTEGER NOT NULL CHECK (minutes BETWEEN 1 AND 1440),
    narrative TEXT NOT NULL CHECK (length(btrim(narrative)) BETWEEN 1 AND 2000),
    -- `lawyer` is time a lawyer recorded by hand; every other kind names the
    -- dated record the proposal was drawn from.
    source_kind TEXT NOT NULL CHECK (source_kind IN ('lawyer', 'communication', 'document', 'deadline', 'efiling')),
    source_id UUID,
    -- The caller's retry key: replaying an add returns the first row.
    request_id UUID NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending_review', 'approved', 'discarded')),
    created_by UUID NOT NULL REFERENCES person(id),
    reviewed_by UUID REFERENCES person(id),
    reviewed_at TIMESTAMPTZ,
    entry_id UUID UNIQUE REFERENCES time_entry(id),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Work dated before the day it was recorded is backfilled.
    backfilled BOOLEAN GENERATED ALWAYS AS (performed_on < (timezone('UTC', inserted_at))::date) STORED,
    UNIQUE (project_id, request_id),
    FOREIGN KEY (project_id, timekeeper_id) REFERENCES person_project_role (project_id, person_id),
    CONSTRAINT time_entry_proposal_source CHECK ((source_kind = 'lawyer') = (source_id IS NULL)),
    CONSTRAINT time_entry_proposal_review_evidence CHECK (
        (status = 'pending_review' AND reviewed_by IS NULL AND reviewed_at IS NULL AND entry_id IS NULL)
        OR (status = 'discarded' AND reviewed_by IS NOT NULL AND reviewed_at IS NOT NULL AND entry_id IS NULL)
        OR (status = 'approved' AND reviewed_by IS NOT NULL AND reviewed_at IS NOT NULL AND entry_id IS NOT NULL)
    )
);
-- One proposal per source record, so a repeated backfill adds nothing.
CREATE UNIQUE INDEX time_entry_proposal_source_record
    ON time_entry_proposal (project_id, source_kind, source_id) WHERE source_id IS NOT NULL;
CREATE INDEX time_entry_proposal_project ON time_entry_proposal (project_id, status, performed_on, id);

-- Proposals are retained. Only a pending proposal changes, and only its
-- performed date, minutes, narrative, and review decision.
CREATE FUNCTION protect_time_entry_proposal() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'Time Entry Proposals are retained; discard one instead';
    END IF;
    IF OLD.status <> 'pending_review' THEN
        RAISE EXCEPTION 'A reviewed Time Entry Proposal keeps its decision';
    END IF;
    IF (to_jsonb(OLD) - ARRAY['performed_on','minutes','narrative','status','reviewed_by','reviewed_at','entry_id','updated_at','backfilled']) IS DISTINCT FROM
       (to_jsonb(NEW) - ARRAY['performed_on','minutes','narrative','status','reviewed_by','reviewed_at','entry_id','updated_at','backfilled']) THEN
        RAISE EXCEPTION 'A Time Entry Proposal keeps its Project, timekeeper, and source';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER time_entry_proposal_retained BEFORE UPDATE OR DELETE ON time_entry_proposal
    FOR EACH ROW EXECUTE FUNCTION protect_time_entry_proposal();
