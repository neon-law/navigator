-- Tracked Git repositories and Git access tokens, mirrored from SurrealDB
-- under their record UUIDs. A token is held only as its SHA-256 hash.
CREATE TABLE git_repository (
    id UUID PRIMARY KEY,
    remote_hash TEXT NOT NULL UNIQUE,
    last_commit_sha TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE git_access_token (
    id UUID PRIMARY KEY,
    person_id UUID NOT NULL REFERENCES person(id) ON DELETE CASCADE,
    project_id UUID REFERENCES project(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    scope TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);
