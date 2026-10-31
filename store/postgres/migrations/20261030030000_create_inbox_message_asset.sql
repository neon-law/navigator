-- SurrealDB remains the write authority; Inbox rows are opt-in mirrors.
CREATE TABLE inbox_message_asset (
    id UUID PRIMARY KEY,
    firm_id UUID NOT NULL REFERENCES firm(id),
    message_id UUID NOT NULL REFERENCES message(id),
    asset_id UUID NOT NULL REFERENCES asset(id),
    filename TEXT NOT NULL,
    position INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    inserted_at TIMESTAMPTZ NOT NULL,
    CONSTRAINT inbox_message_asset_key UNIQUE (message_id, sha256)
);
CREATE INDEX inbox_message_asset_message ON inbox_message_asset (message_id);

CREATE TRIGGER inbox_message_asset_immutable
    BEFORE UPDATE OR DELETE ON inbox_message_asset
    FOR EACH ROW EXECUTE FUNCTION reject_inbox_audit_mutation();
