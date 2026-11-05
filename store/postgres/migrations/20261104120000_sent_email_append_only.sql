-- sent_email becomes PostgreSQL's system of record for outbound mail: one
-- row per send, never changed afterward. `person_id` is the addressee the
-- caller named (no foreign key: the identity mirror can lag a send, and an
-- audit row must never fail on that race). Rows live here for one year; the
-- daily Sent Email Export copies each closed UTC day to Iceberg, and only a
-- day the export ledger confirms may be purged.
ALTER TABLE sent_email ADD COLUMN person_id UUID;
CREATE INDEX sent_email_sent_at ON sent_email (sent_at, id);
CREATE INDEX sent_email_person_sent_at ON sent_email (person_id, sent_at DESC) WHERE person_id IS NOT NULL;
CREATE INDEX sent_email_sg_message_id ON sent_email (sg_message_id) WHERE sg_message_id IS NOT NULL;

-- One row per exported UTC day: what the Iceberg data file holds. A re-run
-- over an unchanged day finds the same count and digest and writes nothing; a
-- day that gained a late row is exported again and its row replaced. The purge
-- stamps `purged_at`, after which the day is never exported again.
CREATE TABLE sent_email_export (
    day DATE PRIMARY KEY,
    row_count BIGINT NOT NULL CHECK (row_count >= 0),
    content_sha256 TEXT NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    data_file TEXT NOT NULL CHECK (data_file <> ''),
    snapshot_id BIGINT NOT NULL,
    metadata_version BIGINT NOT NULL CHECK (metadata_version > 0),
    exported_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    purged_at TIMESTAMPTZ
);

-- UPDATE is always refused. DELETE is refused unless the session names the
-- retention purge, the row is older than one year, and its UTC day is in the
-- export ledger. The purge also checks the ledger's count against the day's
-- rows before deleting any of them.
CREATE FUNCTION sent_email_refuse_mutation() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE'
        AND current_setting('navigator.sent_email_purge', true) = 'on'
        AND OLD.sent_at < now() - interval '1 year'
        AND EXISTS (SELECT 1 FROM sent_email_export WHERE day = (OLD.sent_at AT TIME ZONE 'UTC')::date) THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'sent_email rows are append-only';
END;
$$;
CREATE TRIGGER sent_email_append_only
    BEFORE UPDATE OR DELETE ON sent_email
    FOR EACH ROW EXECUTE FUNCTION sent_email_refuse_mutation();

CREATE FUNCTION sent_email_refuse_truncate() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'sent_email rows are append-only';
END;
$$;
CREATE TRIGGER sent_email_no_truncate
    BEFORE TRUNCATE ON sent_email
    FOR EACH STATEMENT EXECUTE FUNCTION sent_email_refuse_truncate();
