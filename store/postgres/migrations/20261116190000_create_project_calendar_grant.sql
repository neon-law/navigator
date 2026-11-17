-- Direct reader grants managed by Project Calendar reconciliation.
CREATE TABLE project_calendar_grant (
    project_id UUID NOT NULL REFERENCES project(id) ON DELETE CASCADE,
    firm_id UUID NOT NULL REFERENCES firm(id),
    calendar_id TEXT NOT NULL,
    email TEXT NOT NULL,
    notified BOOLEAN NOT NULL DEFAULT false,
    PRIMARY KEY (project_id, firm_id, calendar_id, email)
);
