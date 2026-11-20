-- A captured posted-event history is not permission to activate native callers.
CREATE TABLE durable_object_handover (
    capture_id UUID PRIMARY KEY,
    capture_version INTEGER NOT NULL CHECK (capture_version = 1),
    origin TEXT NOT NULL DEFAULT 'captured_source' CHECK (origin IN ('captured_source', 'fresh_native')),
    source_environment TEXT CHECK (length(source_environment) BETWEEN 1 AND 128),
    capture_sha256 TEXT CHECK (capture_sha256 ~ '^[0-9a-f]{64}$'),
    notation_machines INTEGER NOT NULL CHECK (notation_machines >= 0),
    slack_projects INTEGER NOT NULL CHECK (slack_projects >= 0),
    slack_posted_events INTEGER NOT NULL CHECK (slack_posted_events >= 0),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((origin = 'captured_source' AND source_environment IS NOT NULL AND capture_sha256 IS NOT NULL) OR
           (origin = 'fresh_native' AND source_environment IS NULL AND capture_sha256 IS NULL AND
            notation_machines = 0 AND slack_projects = 0 AND slack_posted_events = 0))
);

CREATE TABLE durable_object_ownership (
    singleton BOOLEAN PRIMARY KEY DEFAULT true CHECK (singleton),
    ownership_epoch UUID NOT NULL UNIQUE,
    capture_id UUID NOT NULL REFERENCES durable_object_handover(capture_id),
    activated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE project_slack_handover (
    project_id UUID PRIMARY KEY REFERENCES project(id) ON DELETE CASCADE,
    ownership_epoch UUID NOT NULL,
    capture_id UUID REFERENCES durable_object_handover(capture_id),
    native_active BOOLEAN NOT NULL DEFAULT false,
    client_view_disposition TEXT NOT NULL
        CHECK (client_view_disposition IN ('posted', 'not_posted', 'unresolved')),
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (capture_id IS NOT NULL OR native_active)
);

CREATE TABLE project_slack_delivery (
    project_id UUID NOT NULL REFERENCES project_slack_handover(project_id) ON DELETE CASCADE,
    event_kind TEXT NOT NULL CHECK (event_kind IN ('main_push', 'client_view')),
    event_key TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (project_id, event_kind, event_key),
    CHECK ((event_kind = 'client_view' AND event_key = 'client-view') OR
           (event_kind = 'main_push' AND event_key ~ '^[0-9a-f]{7,40}:[1-9][0-9]*$'))
);
