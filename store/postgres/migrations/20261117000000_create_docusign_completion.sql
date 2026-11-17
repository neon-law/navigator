CREATE TABLE docusign_completion (
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    account_id UUID NOT NULL,
    envelope_id UUID NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'review', 'filed')),
    review_reason TEXT,
    notified_at TIMESTAMPTZ,
    project_id UUID REFERENCES project(id) ON DELETE SET NULL,
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (firm_id, account_id, envelope_id)
);

CREATE INDEX docusign_completion_pending
    ON docusign_completion (next_attempt_at)
    WHERE status = 'pending';

CREATE TABLE docusign_completion_cursor (
    firm_id UUID NOT NULL REFERENCES firm(id) ON DELETE CASCADE,
    account_id UUID NOT NULL,
    checked_through TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (firm_id, account_id)
);
