-- An asset's notarial acknowledgments, soft deletion, Legal Hold, and purge
-- tombstone, shaped as their SurrealDB source so the document mirror copies
-- them one to one. A soft deletion stamps every revision of a slug chain.
ALTER TABLE asset ADD COLUMN notarization JSONB;
ALTER TABLE asset ADD COLUMN deleted_at TIMESTAMPTZ;
ALTER TABLE asset ADD COLUMN purge_after TIMESTAMPTZ;
ALTER TABLE asset ADD COLUMN deleted_by_person_id UUID REFERENCES person(id);
ALTER TABLE asset ADD COLUMN deleted_commit_sha TEXT;
ALTER TABLE asset ADD COLUMN legal_hold_at TIMESTAMPTZ;
ALTER TABLE asset ADD COLUMN purged_at TIMESTAMPTZ;

-- The dedup probe and the revision chain read, newest id first.
CREATE INDEX asset_sha256 ON asset (sha256_hex);
CREATE INDEX asset_project_slug ON asset (project_id, slug);
