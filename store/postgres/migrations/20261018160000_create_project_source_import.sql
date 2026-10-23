-- A durable request to initialize a Project's source repository from a
-- validated headless source archive. Born in PostgreSQL. The canonical ZIP is
-- the recovery source a retry after worker loss or an unknown forge outcome
-- re-reads; it is cleared once the commit is verified and recorded.
CREATE TABLE project_source_import (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    project_id UUID NOT NULL,
    operation_key TEXT NOT NULL UNIQUE CHECK (operation_key <> ''),
    tree_sha256 TEXT NOT NULL CHECK (tree_sha256 ~ '^[0-9a-f]{64}$'),
    file_count INTEGER NOT NULL CHECK (file_count > 0),
    canonical_zip BYTEA,
    state TEXT NOT NULL CHECK (state IN ('pending', 'committed', 'refused')),
    requested_by_person_id UUID NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL,
    repository_url TEXT,
    commit_sha TEXT CHECK (commit_sha ~ '^[0-9a-f]{40,64}$'),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    finished_at TIMESTAMPTZ,
    CHECK ((state = 'pending') = (canonical_zip IS NOT NULL)),
    CHECK ((state = 'committed') = (commit_sha IS NOT NULL AND repository_url IS NOT NULL AND finished_at IS NOT NULL))
);
CREATE UNIQUE INDEX project_source_import_one_pending ON project_source_import (project_id) WHERE state = 'pending';
CREATE INDEX project_source_import_state ON project_source_import (state, requested_at);
