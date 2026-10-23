-- Project Recovery Archive generations in the private archived-projects bucket.
-- Born in PostgreSQL. A generation is committed only after every expected item
-- was copied, read back, and verified by size and SHA-256, and its
-- content-addressed manifest was written and verified the same way.
CREATE TABLE project_archive_generation (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL,
    project_id UUID NOT NULL,
    operation_key TEXT NOT NULL UNIQUE CHECK (operation_key <> ''),
    state TEXT NOT NULL CHECK (state IN ('pending', 'copying', 'committed')),
    requested_by_person_id UUID NOT NULL,
    requested_at TIMESTAMPTZ NOT NULL,
    source_backup JSONB,
    cutoff_at TIMESTAMPTZ,
    inventory_sha256 TEXT CHECK (inventory_sha256 ~ '^[0-9a-f]{64}$'),
    manifest_key TEXT,
    manifest_sha256 TEXT CHECK (manifest_sha256 ~ '^[0-9a-f]{64}$'),
    manifest_size_bytes BIGINT CHECK (manifest_size_bytes > 0),
    item_count INTEGER CHECK (item_count >= 0),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    last_error TEXT,
    committed_at TIMESTAMPTZ,
    CHECK ((state = 'committed') = (committed_at IS NOT NULL AND manifest_key IS NOT NULL AND manifest_sha256 IS NOT NULL AND manifest_size_bytes IS NOT NULL AND item_count IS NOT NULL AND inventory_sha256 IS NOT NULL AND cutoff_at IS NOT NULL))
);
CREATE INDEX project_archive_generation_project ON project_archive_generation (project_id, state);
CREATE INDEX project_archive_generation_firm ON project_archive_generation (firm_id);

-- One expected item of a generation and its copy checkpoint.
CREATE TABLE project_archive_item (
    generation_id UUID NOT NULL REFERENCES project_archive_generation(id),
    path TEXT NOT NULL CHECK (path <> ''),
    kind TEXT NOT NULL CHECK (kind IN ('document_revision', 'notation_source', 'notation_output', 'restore_metadata', 'source_backup')),
    asset_id UUID,
    source_key TEXT,
    object_key TEXT NOT NULL CHECK (object_key <> ''),
    sha256 TEXT NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes BIGINT NOT NULL CHECK (size_bytes >= 0),
    verified_at TIMESTAMPTZ,
    PRIMARY KEY (generation_id, path)
);
CREATE INDEX project_archive_item_object ON project_archive_item (object_key);

CREATE FUNCTION reject_committed_archive_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.state = 'committed' THEN
        RAISE EXCEPTION 'A committed Project Recovery Archive generation is immutable';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_archive_generation_immutable
    BEFORE UPDATE OR DELETE ON project_archive_generation
    FOR EACH ROW EXECUTE FUNCTION reject_committed_archive_mutation();

CREATE FUNCTION reject_committed_archive_item_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF EXISTS (SELECT 1 FROM project_archive_generation WHERE id = OLD.generation_id AND state = 'committed') THEN
        RAISE EXCEPTION 'Items of a committed Project Recovery Archive generation are immutable';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER project_archive_item_immutable
    BEFORE UPDATE OR DELETE ON project_archive_item
    FOR EACH ROW EXECUTE FUNCTION reject_committed_archive_item_mutation();
