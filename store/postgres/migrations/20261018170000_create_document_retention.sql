-- Soft-deleted document chains in the private archived-projects lane, and the
-- governed retention purge's own accounting. Born in PostgreSQL.

-- One archive of one soft-deleted chain, per soft deletion.
CREATE TABLE document_chain_archive (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    project_id UUID NOT NULL,
    chain_key TEXT NOT NULL CHECK (chain_key <> ''),
    deleted_at TIMESTAMPTZ NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('archiving', 'archived', 'restored', 'purged')),
    requested_at TIMESTAMPTZ NOT NULL,
    archived_at TIMESTAMPTZ,
    active_removed_at TIMESTAMPTZ,
    restored_at TIMESTAMPTZ,
    purged_at TIMESTAMPTZ,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    UNIQUE (project_id, chain_key, deleted_at),
    CHECK (state = 'archiving' OR archived_at IS NOT NULL),
    CHECK (active_removed_at IS NULL OR archived_at IS NOT NULL)
);
CREATE INDEX document_chain_archive_state ON document_chain_archive (state);

-- One revision's verified copy.
CREATE TABLE document_chain_archive_item (
    archive_id UUID NOT NULL REFERENCES document_chain_archive(id),
    asset_id UUID NOT NULL,
    storage_key TEXT NOT NULL,
    object_key TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    verified_at TIMESTAMPTZ,
    PRIMARY KEY (archive_id, asset_id)
);
CREATE INDEX document_chain_archive_item_asset ON document_chain_archive_item (asset_id);

-- One governed purge of one chain past its window, keyed so a retry after a
-- crash joins the same run rather than starting another.
CREATE TABLE retention_purge (
    project_id UUID NOT NULL,
    chain_key TEXT NOT NULL CHECK (chain_key <> ''),
    purge_after TIMESTAMPTZ NOT NULL,
    firm_id UUID NOT NULL,
    authorized_by_person_id UUID NOT NULL,
    started_at TIMESTAMPTZ NOT NULL,
    expunge_record_id UUID,
    finished_at TIMESTAMPTZ,
    PRIMARY KEY (project_id, chain_key, purge_after)
);

-- Archived bytes the retention purge removed, so recovery generations that
-- held them account for the removal instead of reporting corruption.
CREATE TABLE archived_object_purge (
    project_id UUID NOT NULL,
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    purged_at TIMESTAMPTZ NOT NULL,
    expunge_record_id UUID,
    PRIMARY KEY (project_id, sha256)
);
