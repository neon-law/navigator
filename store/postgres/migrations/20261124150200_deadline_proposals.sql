ALTER TABLE project_deadlines DROP CONSTRAINT project_deadlines_authority_kind_check;
ALTER TABLE project_deadlines ADD CONSTRAINT project_deadlines_authority_kind_check CHECK (authority_kind IN ('statute','court_rule','case_law','court_order','contract','internal'));

CREATE TABLE deadline_proposal (
    project_id UUID NOT NULL REFERENCES project(id),
    id UUID NOT NULL,
    revision BIGINT NOT NULL CHECK (revision > 0),
    draft JSONB NOT NULL,
    target_deadline_id UUID REFERENCES project_deadlines(id),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','approved','published','returned')),
    reviewer_id UUID REFERENCES person(id),
    reviewed_at TIMESTAMPTZ,
    deadline_id UUID REFERENCES project_deadlines(id),
    reason TEXT,
    attempt INTEGER NOT NULL DEFAULT 0,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    worker_settled BOOLEAN NOT NULL DEFAULT false,
    PRIMARY KEY (project_id,id,revision),
    CHECK ((reviewer_id IS NULL) = (reviewed_at IS NULL)),
    CHECK ((status = 'pending') = (reviewer_id IS NULL)),
    CHECK (status <> 'published' OR deadline_id IS NOT NULL),
    CHECK ((status = 'returned') = (reason IS NOT NULL))
);
CREATE INDEX deadline_proposal_dispatch ON deadline_proposal(next_attempt_at) WHERE NOT worker_settled;
ALTER TABLE project_deadlines ADD COLUMN calendar_review_pending BOOLEAN NOT NULL DEFAULT false;
CREATE UNIQUE INDEX deadline_proposal_first_deadline ON deadline_proposal(deadline_id) WHERE revision=1 AND deadline_id IS NOT NULL;
