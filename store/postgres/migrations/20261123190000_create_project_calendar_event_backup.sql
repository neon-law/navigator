CREATE TABLE project_calendar_event_backup (
    project_id UUID NOT NULL REFERENCES project(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    calendar_id TEXT NOT NULL,
    resource_href TEXT NOT NULL,
    etag TEXT NOT NULL,
    ics TEXT NOT NULL,
    backed_up_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, firm_id, calendar_id, resource_href, etag)
);

CREATE TABLE project_calendar_retirement (
    project_id UUID NOT NULL REFERENCES project(id),
    firm_id UUID NOT NULL REFERENCES firm(id),
    calendar_id TEXT NOT NULL,
    owner_mailbox TEXT NOT NULL,
    event_versions JSONB NOT NULL,
    content_sha256 TEXT NOT NULL,
    capture_xid TEXT NOT NULL DEFAULT pg_current_xact_id()::text,
    archive_generation UUID,
    delete_etag TEXT,
    deleted_at TIMESTAMPTZ,
    PRIMARY KEY (project_id, firm_id, calendar_id)
);
